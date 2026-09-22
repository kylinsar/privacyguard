//! SQLite 持久化：设置、规则、保护状态、请求日志与模型调用记录。

pub mod pricing;
pub mod report;
pub mod schema;
pub mod settings;
pub mod sink;

pub use pricing::{default_prices, ModelPrice};
pub use report::{UsageReport, UsageTotals};
pub use settings::{Agent, AgentActivation, AppSettings, Persona, ProtectionMode};
pub use sink::StoreSink;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use proxy_core::RequestLog;
use redact::{builtin_rules, IdentityField, Redactor, Rule, Style};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequestFilter {
    pub agent: Option<String>,
    pub provider: Option<String>,
    /// "ok" / "error" / None
    pub outcome: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub search: Option<String>,
    pub only_redacted: bool,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRow {
    pub id: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub entry: String,
    pub agent: String,
    pub provider: String,
    pub api: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub req_bytes: u64,
    pub resp_bytes: u64,
    pub streamed: bool,
    pub redaction_count: u64,
    pub restored_count: u64,
    pub error: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCallRow {
    pub provider: String,
    pub api: String,
    pub model: Option<String>,
    pub response_model: Option<String>,
    pub stream: bool,
    pub message_count: u64,
    pub tool_names: Vec<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub stop_reason: Option<String>,
    pub reasoning_tokens: Option<u64>,
    pub reasoning_effort: Option<String>,
    pub tool_calls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionRow {
    pub rule_id: String,
    pub entity_type: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestDetail {
    pub request: RequestRow,
    pub model_call: Option<ModelCallRow>,
    pub redactions: Vec<RedactionRow>,
    pub warnings: Vec<String>,
    pub redacted_body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub total_requests: u64,
    pub total_redactions: u64,
    pub total_errors: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub by_entity: Vec<(String, u64)>,
    pub by_agent: Vec<(String, u64)>,
    pub by_model: Vec<(String, u64)>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("打开数据库 {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ---------- settings ----------

    pub fn get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.conn.lock();
        let v: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(match v {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let s = serde_json::to_string(value)?;
        self.conn.lock().execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, s],
        )?;
        Ok(())
    }

    pub fn app_settings(&self) -> Result<AppSettings> {
        Ok(self.get_json("app")?.unwrap_or_default())
    }

    pub fn save_app_settings(&self, s: &AppSettings) -> Result<()> {
        self.set_json("app", s)
    }

    // ---------- activation ----------

    pub fn activation(&self, agent: Agent) -> Result<AgentActivation> {
        let conn = self.conn.lock();
        let v: Option<String> = conn
            .query_row(
                "SELECT json FROM activation_state WHERE key = ?1",
                [agent.key()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match v {
            Some(s) => serde_json::from_str(&s)?,
            None => AgentActivation::default(),
        })
    }

    pub fn set_activation(&self, agent: Agent, a: &AgentActivation) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO activation_state(key, json, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
            params![agent.key(), serde_json::to_string(a)?, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// 任意键的状态（如系统 PAC 备份），供平台层使用。
    pub fn get_state<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.conn.lock();
        let v: Option<String> = conn
            .query_row("SELECT json FROM activation_state WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(match v {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn set_state<T: Serialize>(&self, key: &str, v: &T) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO activation_state(key, json, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
            params![key, serde_json::to_string(v)?, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn delete_state(&self, key: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("DELETE FROM activation_state WHERE key = ?1", [key])?;
        Ok(())
    }

    // ---------- rules ----------

    /// 合并后的规则列表：内置规则以代码为准、仅从库中读取启停状态；自定义规则完整来自库。
    pub fn rules(&self) -> Result<Vec<Rule>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id, builtin, enabled, json FROM rules ORDER BY sort, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut overrides: HashMap<String, bool> = HashMap::new();
        let mut custom: Vec<Rule> = Vec::new();
        for row in rows {
            let (id, builtin, enabled, json) = row?;
            if builtin {
                overrides.insert(id, enabled);
            } else if let Ok(mut r) = serde_json::from_str::<Rule>(&json) {
                r.enabled = enabled;
                r.builtin = false;
                custom.push(r);
            }
        }
        let mut out = builtin_rules();
        for r in &mut out {
            if let Some(e) = overrides.get(&r.id) {
                r.enabled = *e;
            }
        }
        out.extend(custom);
        Ok(out)
    }

    pub fn set_rule_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let is_builtin = builtin_rules().iter().any(|r| r.id == id);
        let conn = self.conn.lock();
        if is_builtin {
            conn.execute(
                "INSERT INTO rules(id, builtin, enabled, json, sort, updated_at) VALUES(?1, 1, ?2, '{}', 0, ?3)
                 ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
                params![id, enabled as i64, Utc::now().to_rfc3339()],
            )?;
        } else {
            conn.execute(
                "UPDATE rules SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, enabled as i64, Utc::now().to_rfc3339()],
            )?;
        }
        Ok(())
    }

    pub fn upsert_custom_rule(&self, rule: &Rule) -> Result<()> {
        anyhow::ensure!(!rule.builtin, "内置规则不可修改");
        anyhow::ensure!(!rule.id.starts_with("builtin."), "自定义规则 id 不能以 builtin. 开头");
        redact::RegexDetector::validate(rule)?;
        self.conn.lock().execute(
            "INSERT INTO rules(id, builtin, enabled, json, sort, updated_at) VALUES(?1, 0, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, json = excluded.json, sort = excluded.sort, updated_at = excluded.updated_at",
            params![
                rule.id,
                rule.enabled as i64,
                serde_json::to_string(rule)?,
                rule.priority,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn delete_custom_rule(&self, id: &str) -> Result<()> {
        anyhow::ensure!(!id.starts_with("builtin."), "内置规则不可删除");
        self.conn
            .lock()
            .execute("DELETE FROM rules WHERE id = ?1 AND builtin = 0", [id])?;
        Ok(())
    }

    // ---------- personas ----------

    pub fn personas(&self) -> Result<Vec<Persona>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT json FROM personas ORDER BY sort, updated_at")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(p) = serde_json::from_str::<Persona>(&row?) {
                out.push(p);
            }
        }
        Ok(out)
    }

    pub fn persona(&self, id: &str) -> Result<Option<Persona>> {
        let conn = self.conn.lock();
        let v: Option<String> = conn
            .query_row("SELECT json FROM personas WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(match v {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn upsert_persona(&self, p: &Persona) -> Result<Persona> {
        anyhow::ensure!(!p.id.trim().is_empty(), "身份 id 不能为空");
        let mut p = p.clone();
        p.updated_at = Utc::now().to_rfc3339();
        // 清理字段：去掉空白，保证 id 唯一
        for (i, f) in p.fields.iter_mut().enumerate() {
            if f.id.trim().is_empty() {
                f.id = format!("f{i}");
            }
            f.alias = f.alias.trim().to_string();
            f.real_values = f
                .real_values
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        self.conn.lock().execute(
            "INSERT INTO personas(id, json, sort, updated_at) VALUES(?1, ?2, 0, ?3)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
            params![p.id, serde_json::to_string(&p)?, p.updated_at],
        )?;
        Ok(p)
    }

    pub fn delete_persona(&self, id: &str) -> Result<()> {
        self.conn.lock().execute("DELETE FROM personas WHERE id = ?1", [id])?;
        let mut s = self.app_settings()?;
        if s.active_persona.as_deref() == Some(id) {
            s.active_persona = None;
            self.save_app_settings(&s)?;
        }
        Ok(())
    }

    /// 当前启用身份的字段（未启用或身份不存在时为空）。
    pub fn active_identity(&self) -> Result<Vec<IdentityField>> {
        let s = self.app_settings()?;
        let Some(id) = s.active_persona else { return Ok(Vec::new()) };
        Ok(self.persona(&id)?.map(|p| p.fields).unwrap_or_default())
    }

    /// 脱敏引擎的完整配置：规则 + 身份 + 风格。
    pub fn redaction_config(&self) -> Result<(Vec<Rule>, Vec<IdentityField>, Style)> {
        Ok((self.rules()?, self.active_identity()?, self.app_settings()?.substitution_style))
    }

    pub fn build_redactor(&self, capacity: usize) -> Result<Redactor> {
        let (rules, identity, style) = self.redaction_config()?;
        let r = Redactor::from_rules(&rules, capacity)?;
        r.configure(&rules, &identity, style)?;
        Ok(r)
    }

    pub fn configure_redactor(&self, r: &Redactor) -> Result<()> {
        let (rules, identity, style) = self.redaction_config()?;
        r.configure(&rules, &identity, style)?;
        Ok(())
    }

    // ---------- logs ----------

    pub fn insert_log(&self, log: &RequestLog) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO requests(id, started_at, duration_ms, entry, agent, provider, api, method, host, path,
                status, req_bytes, resp_bytes, streamed, redaction_count, restored_count, error, warnings, ttft_ms, session_id)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                log.id,
                log.started_at.to_rfc3339(),
                log.duration_ms as i64,
                serde_json::to_value(log.entry)?.as_str().unwrap_or("mitm"),
                log.agent,
                log.provider.as_str(),
                serde_json::to_value(log.api)?.as_str().unwrap_or("unknown"),
                log.method,
                log.host,
                log.path,
                log.status.map(|s| s as i64),
                log.req_bytes as i64,
                log.resp_bytes as i64,
                log.streamed as i64,
                log.redaction_count() as i64,
                log.restored_count as i64,
                log.error,
                if log.warnings.is_empty() { None } else { Some(serde_json::to_string(&log.warnings)?) },
                log.ttft_ms.map(|v| v as i64),
                log.session_id,
            ],
        )?;
        if let Some(meta) = &log.request_meta {
            let usage = log.usage.clone().unwrap_or_default();
            tx.execute(
                "INSERT OR REPLACE INTO model_calls(request_id, provider, api, model, response_model, stream, message_count,
                    tool_names, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, stop_reason,
                    reasoning_tokens, reasoning_effort, tool_calls)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    log.id,
                    log.provider.as_str(),
                    serde_json::to_value(log.api)?.as_str().unwrap_or("unknown"),
                    meta.model,
                    usage.response_model,
                    meta.stream as i64,
                    meta.message_count as i64,
                    serde_json::to_string(&meta.tool_names)?,
                    usage.input_tokens.map(|v| v as i64),
                    usage.output_tokens.map(|v| v as i64),
                    usage.cache_read_tokens.map(|v| v as i64),
                    usage.cache_write_tokens.map(|v| v as i64),
                    usage.stop_reason,
                    usage.reasoning_tokens.map(|v| v as i64),
                    meta.reasoning_effort,
                    serde_json::to_string(&usage.tool_calls)?,
                ],
            )?;
        }
        for e in &log.redaction_events {
            tx.execute(
                "INSERT INTO redaction_events(request_id, rule_id, entity_type, count) VALUES(?1, ?2, ?3, ?4)",
                params![log.id, e.rule_id, e.entity_type, e.count as i64],
            )?;
        }
        if let Some(body) = &log.redacted_body {
            tx.execute(
                "INSERT OR REPLACE INTO request_bodies(request_id, redacted_body) VALUES(?1, ?2)",
                params![log.id, body],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_requests(&self, f: &RequestFilter) -> Result<Vec<RequestRow>> {
        let mut sql = String::from(
            "SELECT r.id, r.started_at, r.duration_ms, r.entry, r.agent, r.provider, r.api, r.method, r.host, r.path,
                    r.status, r.req_bytes, r.resp_bytes, r.streamed, r.redaction_count, r.restored_count, r.error,
                    m.model, m.input_tokens, m.output_tokens, r.ttft_ms, r.session_id
             FROM requests r LEFT JOIN model_calls m ON m.request_id = r.id WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(a) = &f.agent {
            sql.push_str(" AND r.agent = ?");
            args.push(Box::new(a.clone()));
        }
        if let Some(p) = &f.provider {
            sql.push_str(" AND r.provider = ?");
            args.push(Box::new(p.clone()));
        }
        match f.outcome.as_deref() {
            Some("ok") => sql.push_str(" AND r.error IS NULL AND (r.status IS NULL OR r.status < 400)"),
            Some("error") => sql.push_str(" AND (r.error IS NOT NULL OR r.status >= 400)"),
            _ => {}
        }
        if let Some(s) = &f.since {
            sql.push_str(" AND r.started_at >= ?");
            args.push(Box::new(s.to_rfc3339()));
        }
        if let Some(u) = &f.until {
            sql.push_str(" AND r.started_at <= ?");
            args.push(Box::new(u.to_rfc3339()));
        }
        if let Some(q) = &f.search {
            if !q.trim().is_empty() {
                sql.push_str(" AND (r.path LIKE ? OR r.host LIKE ? OR m.model LIKE ?)");
                let like = format!("%{}%", q.trim());
                args.push(Box::new(like.clone()));
                args.push(Box::new(like.clone()));
                args.push(Box::new(like));
            }
        }
        if f.only_redacted {
            sql.push_str(" AND r.redaction_count > 0");
        }
        sql.push_str(" ORDER BY r.started_at DESC LIMIT ? OFFSET ?");
        args.push(Box::new(if f.limit == 0 { 100 } else { f.limit.min(1000) } as i64));
        args.push(Box::new(f.offset as i64));

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), row_to_request)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn request_detail(&self, id: &str) -> Result<Option<RequestDetail>> {
        let conn = self.conn.lock();
        let request = conn
            .query_row(
                "SELECT r.id, r.started_at, r.duration_ms, r.entry, r.agent, r.provider, r.api, r.method, r.host, r.path,
                        r.status, r.req_bytes, r.resp_bytes, r.streamed, r.redaction_count, r.restored_count, r.error,
                        m.model, m.input_tokens, m.output_tokens, r.ttft_ms, r.session_id
                 FROM requests r LEFT JOIN model_calls m ON m.request_id = r.id WHERE r.id = ?1",
                [id],
                row_to_request,
            )
            .optional()?;
        let Some(request) = request else { return Ok(None) };

        let warnings: Vec<String> = conn
            .query_row("SELECT warnings FROM requests WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0))?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let model_call = conn
            .query_row(
                "SELECT provider, api, model, response_model, stream, message_count, tool_names, input_tokens,
                        output_tokens, cache_read_tokens, cache_write_tokens, stop_reason,
                        reasoning_tokens, reasoning_effort, tool_calls
                 FROM model_calls WHERE request_id = ?1",
                [id],
                |r| {
                    Ok(ModelCallRow {
                        provider: r.get(0)?,
                        api: r.get(1)?,
                        model: r.get(2)?,
                        response_model: r.get(3)?,
                        stream: r.get::<_, i64>(4)? != 0,
                        message_count: r.get::<_, i64>(5)? as u64,
                        tool_names: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                        input_tokens: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                        output_tokens: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                        cache_read_tokens: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                        cache_write_tokens: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                        stop_reason: r.get(11)?,
                        reasoning_tokens: r.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                        reasoning_effort: r.get(13)?,
                        tool_calls: serde_json::from_str(&r.get::<_, String>(14)?).unwrap_or_default(),
                    })
                },
            )
            .optional()?;

        let mut stmt = conn.prepare("SELECT rule_id, entity_type, count FROM redaction_events WHERE request_id = ?1")?;
        let redactions = stmt
            .query_map([id], |r| {
                Ok(RedactionRow {
                    rule_id: r.get(0)?,
                    entity_type: r.get(1)?,
                    count: r.get::<_, i64>(2)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let redacted_body: Option<String> = conn
            .query_row("SELECT redacted_body FROM request_bodies WHERE request_id = ?1", [id], |r| r.get(0))
            .optional()?;

        Ok(Some(RequestDetail {
            request,
            model_call,
            redactions,
            warnings,
            redacted_body,
        }))
    }

    pub fn stats(&self, since: Option<DateTime<Utc>>) -> Result<Stats> {
        let conn = self.conn.lock();
        let since_s = since.map(|s| s.to_rfc3339()).unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
        let mut st = Stats::default();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(redaction_count),0),
                    COALESCE(SUM(CASE WHEN error IS NOT NULL OR status >= 400 THEN 1 ELSE 0 END),0)
             FROM requests WHERE started_at >= ?1",
            [&since_s],
            |r| {
                st.total_requests = r.get::<_, i64>(0)? as u64;
                st.total_redactions = r.get::<_, i64>(1)? as u64;
                st.total_errors = r.get::<_, i64>(2)? as u64;
                Ok(())
            },
        )?;
        conn.query_row(
            "SELECT COALESCE(SUM(m.input_tokens),0), COALESCE(SUM(m.output_tokens),0)
             FROM model_calls m JOIN requests r ON r.id = m.request_id WHERE r.started_at >= ?1",
            [&since_s],
            |r| {
                st.total_input_tokens = r.get::<_, i64>(0)? as u64;
                st.total_output_tokens = r.get::<_, i64>(1)? as u64;
                Ok(())
            },
        )?;
        let mut stmt = conn.prepare(
            "SELECT e.entity_type, SUM(e.count) FROM redaction_events e JOIN requests r ON r.id = e.request_id
             WHERE r.started_at >= ?1 GROUP BY e.entity_type ORDER BY 2 DESC",
        )?;
        st.by_entity = stmt
            .query_map([&since_s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<Result<_, _>>()?;
        let mut stmt = conn.prepare(
            "SELECT agent, COUNT(*) FROM requests WHERE started_at >= ?1 GROUP BY agent ORDER BY 2 DESC",
        )?;
        st.by_agent = stmt
            .query_map([&since_s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<Result<_, _>>()?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(m.model,'?'), COUNT(*) FROM model_calls m JOIN requests r ON r.id = m.request_id
             WHERE r.started_at >= ?1 GROUP BY m.model ORDER BY 2 DESC LIMIT 10",
        )?;
        st.by_model = stmt
            .query_map([&since_s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<Result<_, _>>()?;
        Ok(st)
    }

    pub fn purge_before(&self, before: DateTime<Utc>) -> Result<usize> {
        let n = self
            .conn
            .lock()
            .execute("DELETE FROM requests WHERE started_at < ?1", [before.to_rfc3339()])?;
        Ok(n)
    }

    pub fn clear_logs(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch("DELETE FROM requests; VACUUM;")?;
        Ok(())
    }
}

fn row_to_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRow> {
    Ok(RequestRow {
        id: r.get(0)?,
        started_at: r.get(1)?,
        duration_ms: r.get::<_, i64>(2)? as u64,
        entry: r.get(3)?,
        agent: r.get(4)?,
        provider: r.get(5)?,
        api: r.get(6)?,
        method: r.get(7)?,
        host: r.get(8)?,
        path: r.get(9)?,
        status: r.get::<_, Option<i64>>(10)?.map(|s| s as u16),
        req_bytes: r.get::<_, i64>(11)? as u64,
        resp_bytes: r.get::<_, i64>(12)? as u64,
        streamed: r.get::<_, i64>(13)? != 0,
        redaction_count: r.get::<_, i64>(14)? as u64,
        restored_count: r.get::<_, i64>(15)? as u64,
        error: r.get(16)?,
        model: r.get(17)?,
        input_tokens: r.get::<_, Option<i64>>(18)?.map(|v| v as u64),
        output_tokens: r.get::<_, Option<i64>>(19)?.map(|v| v as u64),
        ttft_ms: r.get::<_, Option<i64>>(20)?.map(|v| v as u64),
        session_id: r.get(21)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use providers::{ApiKind, Provider};

    #[test]
    fn persona_roundtrip_and_redactor() {
        let s = Store::in_memory().unwrap();
        let p = Persona {
            id: "work".into(),
            name: "工作身份".into(),
            fields: vec![IdentityField {
                id: "".into(),
                label: "姓名".into(),
                entity_type: "NAME".into(),
                real_values: vec![" 王大锤 ".into(), "".into()],
                alias: " 陈小明 ".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let saved = s.upsert_persona(&p).unwrap();
        assert_eq!(saved.fields[0].id, "f0");
        assert_eq!(saved.fields[0].real_values, vec!["王大锤"]);
        assert_eq!(saved.fields[0].alias, "陈小明");
        // 未启用时身份为空
        assert!(s.active_identity().unwrap().is_empty());
        let mut st = s.app_settings().unwrap();
        st.active_persona = Some("work".into());
        st.substitution_style = Style::Synthetic;
        s.save_app_settings(&st).unwrap();
        assert_eq!(s.active_identity().unwrap().len(), 1);
        let r = s.build_redactor(100).unwrap();
        let o = r.redact_text("王大锤 a@b.io");
        assert!(o.text.starts_with("陈小明 "));
        assert!(!o.text.contains("PG_") && !o.text.contains("a@b.io"));
        // 删除身份会同时清掉启用状态
        s.delete_persona("work").unwrap();
        assert!(s.personas().unwrap().is_empty());
        assert_eq!(s.app_settings().unwrap().active_persona, None);
    }

    #[test]
    fn rules_merge_builtin_overrides() {
        let s = Store::in_memory().unwrap();
        let before = s.rules().unwrap();
        assert!(before.iter().any(|r| r.id == "builtin.email" && r.enabled));
        s.set_rule_enabled("builtin.email", false).unwrap();
        let after = s.rules().unwrap();
        assert!(after.iter().any(|r| r.id == "builtin.email" && !r.enabled));
        let mut custom = Rule::regex("custom.emp", "员工号", "EMP_ID", r"EMP-\d{6}");
        custom.priority = 10;
        s.upsert_custom_rule(&custom).unwrap();
        assert!(s.rules().unwrap().iter().any(|r| r.id == "custom.emp"));
        s.delete_custom_rule("custom.emp").unwrap();
        assert!(!s.rules().unwrap().iter().any(|r| r.id == "custom.emp"));
    }

    #[test]
    fn log_roundtrip_and_stats() {
        let s = Store::in_memory().unwrap();
        let log = RequestLog {
            id: "r1".into(),
            started_at: Utc::now(),
            duration_ms: 12,
            agent: "claude".into(),
            provider: Provider::Anthropic,
            api: ApiKind::AnthropicMessages,
            method: "POST".into(),
            host: "api.anthropic.com".into(),
            path: "/v1/messages".into(),
            status: Some(200),
            redaction_events: vec![redact::RedactionEvent {
                rule_id: "builtin.email".into(),
                entity_type: "EMAIL".into(),
                count: 2,
            }],
            request_meta: Some(providers::RequestMeta {
                model: Some("claude-x".into()),
                stream: true,
                tool_names: vec!["Read".into()],
                message_count: 3,
                session_id: Some("sess-1".into()),
                reasoning_effort: None,
            }),
            usage: Some(providers::UsageMeta {
                input_tokens: Some(100),
                output_tokens: Some(20),
                cache_read_tokens: Some(50),
                reasoning_tokens: Some(5),
                tool_calls: vec!["Read".into(), "Bash".into(), "Read".into()],
                ..Default::default()
            }),
            ttft_ms: Some(4),
            session_id: Some("sess-1".into()),
            ..Default::default()
        };
        s.insert_log(&log).unwrap();
        let rows = s.list_requests(&RequestFilter::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("claude-x"));
        let d = s.request_detail("r1").unwrap().unwrap();
        assert_eq!(d.redactions[0].count, 2);
        assert_eq!(d.model_call.unwrap().tool_names, vec!["Read"]);
        let st = s.stats(None).unwrap();
        assert_eq!(st.total_redactions, 2);
        assert_eq!(st.total_input_tokens, 100);
        assert_eq!(st.by_entity[0].0, "EMAIL");

        // 使用报表
        let prices = vec![ModelPrice {
            model_prefix: "claude".into(),
            input_per_m: 3.0,
            output_per_m: 15.0,
            cache_read_per_m: 0.3,
            cache_write_per_m: 0.0,
        }];
        let since = Utc::now() - chrono::Duration::hours(1);
        let rep = s.usage_report(since, 300, &prices).unwrap();
        assert_eq!(rep.totals.requests, 1);
        assert_eq!(rep.totals.model_calls, 1);
        assert_eq!(rep.totals.tool_calls, 3);
        assert_eq!(rep.totals.reasoning_tokens, 5);
        assert_eq!(rep.totals.cache_read_tokens, 50);
        assert!((rep.totals.priced_ratio - 1.0).abs() < 1e-9);
        let expect = 100.0 / 1e6 * 3.0 + 20.0 / 1e6 * 15.0 + 50.0 / 1e6 * 0.3;
        assert!((rep.totals.cost_usd - expect).abs() < 1e-12);
        assert_eq!(rep.previous.requests, 0);
        assert_eq!(rep.by_model[0].model, "claude-x");
        assert_eq!(rep.by_model[0].p50_ttft_ms, Some(4));
        assert!(rep.by_model[0].cache_hit_rate > 0.33 && rep.by_model[0].cache_hit_rate < 0.34);
        assert_eq!(rep.tools, vec![("Read".to_string(), 2), ("Bash".to_string(), 1)]);
        assert_eq!(rep.session_count, 1);
        assert_eq!(rep.sessions[0].session_id, "sess-1");
        assert_eq!(rep.sessions[0].tool_calls, 3);
        assert_eq!(rep.by_agent[0].key, "claude");
        assert_eq!(rep.series.iter().map(|b| b.requests).sum::<u64>(), 1);
        // 一小时 / 5 分钟 = 12~13 个桶，且连续铺满
        assert!(rep.series.len() >= 12 && rep.series.len() <= 14, "{}", rep.series.len());
        assert_eq!(rep.heatmap.len(), 7);
        assert_eq!(rep.by_hour.iter().sum::<u64>(), 1);
        assert_eq!(rep.latency.p50_ms, 12);
    }
}
