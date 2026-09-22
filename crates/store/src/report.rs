//! 使用监控报表：在一段时间窗口内按时间桶 / 模型 / 工具 / 会话 / 时段聚合。
//!
//! 数据量是本机个人使用级别（每天几百到几千条），因此一次扫描窗口内的行，在 Rust 里聚合，
//! 比拼十几条 SQL 更简单也更容易保持一致。

use crate::pricing::{estimate_cost, ModelPrice};
use crate::Store;
use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageTotals {
    pub requests: u64,
    pub model_calls: u64,
    pub errors: u64,
    pub streamed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub redactions: u64,
    pub tool_calls: u64,
    pub req_bytes: u64,
    pub resp_bytes: u64,
    pub cost_usd: f64,
    /// 有定价的调用占比（0-1），用于提示成本估算的覆盖度
    pub priced_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageBucket {
    /// 桶起点（UTC RFC3339）
    pub ts: String,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub redactions: u64,
    pub tool_calls: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelUsage {
    pub model: String,
    pub provider: String,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub tool_calls: u64,
    pub cost_usd: f64,
    pub priced: bool,
    pub avg_duration_ms: u64,
    pub p50_duration_ms: u64,
    pub p95_duration_ms: u64,
    pub p50_ttft_ms: Option<u64>,
    /// 缓存命中率 = cache_read / (input + cache_read)（Anthropic）或 cache_read / input（OpenAI）
    pub cache_hit_rate: f64,
    /// 平均每次输出 token / 秒（粗略吞吐）
    pub output_tps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GroupUsage {
    pub key: String,
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionUsage {
    pub session_id: String,
    pub agent: String,
    pub model: String,
    pub first_seen: String,
    pub last_seen: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub tool_calls: u64,
    pub redactions: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LatencyStats {
    pub avg_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub p50_ttft_ms: Option<u64>,
    pub p95_ttft_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageReport {
    pub since: String,
    pub until: String,
    pub bucket_secs: u64,
    pub totals: UsageTotals,
    /// 前一个等长窗口的汇总，用于环比
    pub previous: UsageTotals,
    pub series: Vec<UsageBucket>,
    pub by_model: Vec<ModelUsage>,
    pub by_agent: Vec<GroupUsage>,
    pub by_provider: Vec<GroupUsage>,
    pub by_api: Vec<GroupUsage>,
    /// [周几 0=周一 .. 6=周日][小时 0..23] 请求数（本地时间）
    pub heatmap: Vec<Vec<u64>>,
    /// 按本地小时（0..23）的请求数
    pub by_hour: Vec<u64>,
    pub tools: Vec<(String, u64)>,
    pub stop_reasons: Vec<(String, u64)>,
    pub status_codes: Vec<(String, u64)>,
    pub sessions: Vec<SessionUsage>,
    pub session_count: u64,
    pub latency: LatencyStats,
}

struct Row {
    started_at: DateTime<Utc>,
    duration_ms: u64,
    ttft_ms: Option<u64>,
    agent: String,
    provider: String,
    api: String,
    status: Option<u16>,
    error: bool,
    streamed: bool,
    redactions: u64,
    req_bytes: u64,
    resp_bytes: u64,
    session_id: Option<String>,
    has_call: bool,
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
    stop_reason: Option<String>,
    tool_calls: Vec<String>,
    cost: Option<f64>,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

impl Store {
    /// 生成使用报表。`since`..now 为主窗口，`bucket_secs` 为时间序列粒度。
    pub fn usage_report(&self, since: DateTime<Utc>, bucket_secs: u64, prices: &[ModelPrice]) -> Result<UsageReport> {
        let now = Utc::now();
        let window = now - since;
        let prev_since = since - window;
        let rows = self.scan_rows(prev_since, prices)?;
        let bucket_secs = bucket_secs.max(60);

        let mut report = UsageReport {
            since: since.to_rfc3339(),
            until: now.to_rfc3339(),
            bucket_secs,
            heatmap: vec![vec![0; 24]; 7],
            by_hour: vec![0; 24],
            ..Default::default()
        };

        // 预先铺满时间桶，保证图表连续
        let mut buckets: BTreeMap<i64, UsageBucket> = BTreeMap::new();
        let first_bucket = since.timestamp() - since.timestamp().rem_euclid(bucket_secs as i64);
        let mut t = first_bucket;
        while t <= now.timestamp() {
            buckets.insert(
                t,
                UsageBucket {
                    ts: DateTime::<Utc>::from_timestamp(t, 0).map(|d| d.to_rfc3339()).unwrap_or_default(),
                    ..Default::default()
                },
            );
            t += bucket_secs as i64;
        }

        let mut models: HashMap<String, (ModelUsage, Vec<u64>, Vec<u64>, u64 /*duration sum*/, f64 /*out secs*/)> =
            HashMap::new();
        let mut agents: BTreeMap<String, GroupUsage> = BTreeMap::new();
        let mut providers: BTreeMap<String, GroupUsage> = BTreeMap::new();
        let mut apis: BTreeMap<String, GroupUsage> = BTreeMap::new();
        let mut tools: HashMap<String, u64> = HashMap::new();
        let mut stops: HashMap<String, u64> = HashMap::new();
        let mut statuses: HashMap<String, u64> = HashMap::new();
        let mut sessions: HashMap<String, SessionUsage> = HashMap::new();
        let mut durations: Vec<u64> = Vec::new();
        let mut ttfts: Vec<u64> = Vec::new();
        let mut priced_calls = 0u64;
        let mut prev_priced_calls = 0u64;

        for r in &rows {
            let in_window = r.started_at >= since;
            let totals = if in_window { &mut report.totals } else { &mut report.previous };
            add_totals(totals, r);
            if r.cost.is_some() {
                if in_window {
                    priced_calls += 1;
                } else {
                    prev_priced_calls += 1;
                }
            }
            if !in_window {
                continue;
            }

            // 时间桶
            let ts = r.started_at.timestamp();
            let key = ts - ts.rem_euclid(bucket_secs as i64);
            let b = buckets.entry(key).or_insert_with(|| UsageBucket {
                ts: DateTime::<Utc>::from_timestamp(key, 0).map(|d| d.to_rfc3339()).unwrap_or_default(),
                ..Default::default()
            });
            b.requests += 1;
            b.errors += r.error as u64;
            b.input_tokens += r.input;
            b.output_tokens += r.output;
            b.cache_read_tokens += r.cache_read;
            b.reasoning_tokens += r.reasoning;
            b.redactions += r.redactions;
            b.tool_calls += r.tool_calls.len() as u64;
            b.cost_usd += r.cost.unwrap_or(0.0);

            // 时段
            let local = r.started_at.with_timezone(&Local);
            let wd = local.weekday().num_days_from_monday() as usize;
            let hr = local.hour() as usize;
            report.heatmap[wd][hr] += 1;
            report.by_hour[hr] += 1;

            // 延迟
            durations.push(r.duration_ms);
            if let Some(t) = r.ttft_ms {
                ttfts.push(t);
            }

            // 分组
            for (map, key) in [
                (&mut agents, r.agent.clone()),
                (&mut providers, r.provider.clone()),
                (&mut apis, r.api.clone()),
            ] {
                let g = map.entry(key.clone()).or_insert_with(|| GroupUsage {
                    key,
                    ..Default::default()
                });
                g.requests += 1;
                g.errors += r.error as u64;
                g.input_tokens += r.input;
                g.output_tokens += r.output;
                g.cost_usd += r.cost.unwrap_or(0.0);
            }
            if let Some(s) = r.status {
                *statuses.entry(s.to_string()).or_insert(0) += 1;
            } else if r.error {
                *statuses.entry("network".into()).or_insert(0) += 1;
            }

            if !r.has_call {
                continue;
            }
            // 模型
            let e = models.entry(r.model.clone()).or_insert_with(|| {
                (
                    ModelUsage {
                        model: r.model.clone(),
                        provider: r.provider.clone(),
                        ..Default::default()
                    },
                    Vec::new(),
                    Vec::new(),
                    0,
                    0.0,
                )
            });
            let m = &mut e.0;
            m.requests += 1;
            m.errors += r.error as u64;
            m.input_tokens += r.input;
            m.output_tokens += r.output;
            m.cache_read_tokens += r.cache_read;
            m.cache_write_tokens += r.cache_write;
            m.reasoning_tokens += r.reasoning;
            m.tool_calls += r.tool_calls.len() as u64;
            if let Some(c) = r.cost {
                m.cost_usd += c;
                m.priced = true;
            }
            e.1.push(r.duration_ms);
            if let Some(t) = r.ttft_ms {
                e.2.push(t);
            }
            e.3 += r.duration_ms;
            if r.output > 0 && r.duration_ms > 0 {
                e.4 += r.duration_ms as f64 / 1000.0;
            }

            for t in &r.tool_calls {
                *tools.entry(t.clone()).or_insert(0) += 1;
            }
            if let Some(s) = &r.stop_reason {
                *stops.entry(s.clone()).or_insert(0) += 1;
            }
            if let Some(sid) = &r.session_id {
                let s = sessions.entry(sid.clone()).or_insert_with(|| SessionUsage {
                    session_id: sid.clone(),
                    agent: r.agent.clone(),
                    model: r.model.clone(),
                    first_seen: r.started_at.to_rfc3339(),
                    last_seen: r.started_at.to_rfc3339(),
                    ..Default::default()
                });
                s.requests += 1;
                s.input_tokens += r.input;
                s.output_tokens += r.output;
                s.cache_read_tokens += r.cache_read;
                s.tool_calls += r.tool_calls.len() as u64;
                s.redactions += r.redactions;
                s.cost_usd += r.cost.unwrap_or(0.0);
                if r.started_at.to_rfc3339() > s.last_seen {
                    s.last_seen = r.started_at.to_rfc3339();
                    s.model = r.model.clone();
                }
                if r.started_at.to_rfc3339() < s.first_seen {
                    s.first_seen = r.started_at.to_rfc3339();
                }
            }
        }

        report.totals.priced_ratio = ratio(priced_calls, report.totals.model_calls);
        report.previous.priced_ratio = ratio(prev_priced_calls, report.previous.model_calls);
        report.series = buckets.into_values().collect();

        report.by_model = models
            .into_values()
            .map(|(mut m, mut d, mut t, sum, out_secs)| {
                d.sort_unstable();
                t.sort_unstable();
                m.avg_duration_ms = if d.is_empty() { 0 } else { sum / d.len() as u64 };
                m.p50_duration_ms = percentile(&d, 0.5);
                m.p95_duration_ms = percentile(&d, 0.95);
                m.p50_ttft_ms = if t.is_empty() { None } else { Some(percentile(&t, 0.5)) };
                let denom = if m.provider == "openai" {
                    m.input_tokens
                } else {
                    m.input_tokens + m.cache_read_tokens
                };
                m.cache_hit_rate = ratio(m.cache_read_tokens, denom);
                m.output_tps = if out_secs > 0.0 { m.output_tokens as f64 / out_secs } else { 0.0 };
                m
            })
            .collect();
        report.by_model.sort_by(|a, b| b.requests.cmp(&a.requests));

        report.by_agent = agents.into_values().collect();
        report.by_provider = providers.into_values().collect();
        report.by_api = apis.into_values().collect();
        report.tools = sorted_desc(tools);
        report.stop_reasons = sorted_desc(stops);
        report.status_codes = sorted_desc(statuses);

        report.session_count = sessions.len() as u64;
        let mut sess: Vec<SessionUsage> = sessions.into_values().collect();
        sess.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        sess.truncate(30);
        report.sessions = sess;

        durations.sort_unstable();
        ttfts.sort_unstable();
        report.latency = LatencyStats {
            avg_ms: if durations.is_empty() {
                0
            } else {
                durations.iter().sum::<u64>() / durations.len() as u64
            },
            p50_ms: percentile(&durations, 0.5),
            p95_ms: percentile(&durations, 0.95),
            p99_ms: percentile(&durations, 0.99),
            p50_ttft_ms: if ttfts.is_empty() { None } else { Some(percentile(&ttfts, 0.5)) },
            p95_ttft_ms: if ttfts.is_empty() { None } else { Some(percentile(&ttfts, 0.95)) },
        };
        Ok(report)
    }

    fn scan_rows(&self, since: DateTime<Utc>, prices: &[ModelPrice]) -> Result<Vec<Row>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT r.started_at, r.duration_ms, r.ttft_ms, r.agent, r.provider, r.api, r.status, r.error,
                    r.streamed, r.redaction_count, r.req_bytes, r.resp_bytes, r.session_id,
                    m.request_id, m.model, m.response_model, m.input_tokens, m.output_tokens,
                    m.cache_read_tokens, m.cache_write_tokens, m.reasoning_tokens, m.stop_reason, m.tool_calls
             FROM requests r LEFT JOIN model_calls m ON m.request_id = r.id
             WHERE r.started_at >= ?1 ORDER BY r.started_at",
        )?;
        let rows = stmt.query_map(params![since.to_rfc3339()], |r| {
            let started: String = r.get(0)?;
            let status: Option<i64> = r.get(6)?;
            let error: Option<String> = r.get(7)?;
            let has_call = r.get::<_, Option<String>>(13)?.is_some();
            let model: Option<String> = r.get(14)?;
            let response_model: Option<String> = r.get(15)?;
            let tool_calls: Option<String> = r.get(22)?;
            Ok((
                started,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                status,
                error,
                r.get::<_, i64>(8)? != 0,
                r.get::<_, i64>(9)? as u64,
                r.get::<_, i64>(10)? as u64,
                r.get::<_, i64>(11)? as u64,
                r.get::<_, Option<String>>(12)?,
                has_call,
                response_model.or(model),
                r.get::<_, Option<i64>>(16)?.unwrap_or(0) as u64,
                r.get::<_, Option<i64>>(17)?.unwrap_or(0) as u64,
                r.get::<_, Option<i64>>(18)?.unwrap_or(0) as u64,
                r.get::<_, Option<i64>>(19)?.unwrap_or(0) as u64,
                r.get::<_, Option<i64>>(20)?.unwrap_or(0) as u64,
                r.get::<_, Option<String>>(21)?,
                tool_calls,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                started,
                duration_ms,
                ttft_ms,
                agent,
                provider,
                api,
                status,
                error,
                streamed,
                redactions,
                req_bytes,
                resp_bytes,
                session_id,
                has_call,
                model,
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
                stop_reason,
                tool_calls,
            ) = row?;
            let Ok(started_at) = DateTime::parse_from_rfc3339(&started) else { continue };
            let started_at = started_at.with_timezone(&Utc);
            let model = model.unwrap_or_else(|| "(unknown)".into());
            let is_error = error.is_some() || status.is_some_and(|s| s >= 400);
            let cost = if has_call {
                estimate_cost(prices, &provider, &model, input, output, cache_read, cache_write)
            } else {
                None
            };
            out.push(Row {
                started_at,
                duration_ms,
                ttft_ms,
                agent,
                provider,
                api,
                status: status.map(|s| s as u16),
                error: is_error,
                streamed,
                redactions,
                req_bytes,
                resp_bytes,
                session_id,
                has_call,
                model,
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
                stop_reason,
                tool_calls: tool_calls
                    .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                    .unwrap_or_default(),
                cost,
            });
        }
        Ok(out)
    }
}

fn add_totals(t: &mut UsageTotals, r: &Row) {
    t.requests += 1;
    t.model_calls += r.has_call as u64;
    t.errors += r.error as u64;
    t.streamed += r.streamed as u64;
    t.input_tokens += r.input;
    t.output_tokens += r.output;
    t.cache_read_tokens += r.cache_read;
    t.cache_write_tokens += r.cache_write;
    t.reasoning_tokens += r.reasoning;
    t.redactions += r.redactions;
    t.tool_calls += r.tool_calls.len() as u64;
    t.req_bytes += r.req_bytes;
    t.resp_bytes += r.resp_bytes;
    t.cost_usd += r.cost.unwrap_or(0.0);
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64
    }
}

fn sorted_desc(m: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}
