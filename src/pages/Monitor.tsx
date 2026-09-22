import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert, Button, Card, Col, Drawer, Empty, InputNumber, Input, Popconfirm, Row, Segmented, Space, Statistic, Table, Tag,
  Tooltip as AntTooltip, Typography, App as AntApp,
} from "antd";
import { DollarOutlined, ReloadOutlined, PlusOutlined, DeleteOutlined, ThunderboltOutlined } from "@ant-design/icons";
import {
  Area, AreaChart, Bar, BarChart, CartesianGrid, Cell, ComposedChart, Legend, Line, Pie, PieChart, ResponsiveContainer,
  Tooltip, XAxis, YAxis,
} from "recharts";
import {
  api, onRequest, type ModelPrice, type ModelUsage, type ProxyStatus, type RequestLogEvent, type SessionUsage,
  type UsageReport, type UsageTotals, AGENT_LABEL,
} from "../api";

type RangeKey = "1h" | "24h" | "7d" | "30d";
const RANGES: Record<RangeKey, { hours: number; bucket: number; label: string }> = {
  "1h": { hours: 1, bucket: 60, label: "1 小时" },
  "24h": { hours: 24, bucket: 900, label: "24 小时" },
  "7d": { hours: 24 * 7, bucket: 3600 * 3, label: "7 天" },
  "30d": { hours: 24 * 30, bucket: 3600 * 12, label: "30 天" },
};

const COLORS = ["#2563eb", "#16a34a", "#f59e0b", "#dc2626", "#7c3aed", "#0891b2", "#db2777", "#65a30d", "#ea580c", "#475569"];
const WEEKDAYS = ["一", "二", "三", "四", "五", "六", "日"];

function fmtNum(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e4) return (n / 1e3).toFixed(1) + "k";
  return n.toLocaleString();
}
function fmtCost(v: number | null | undefined): string {
  if (v == null) return "—";
  if (v === 0) return "$0";
  if (v < 0.01) return "$" + v.toFixed(4);
  return "$" + v.toFixed(2);
}
function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return "—";
  if (ms >= 1000) return (ms / 1000).toFixed(1) + " s";
  return ms + " ms";
}
function fmtBytes(b: number): string {
  if (b >= 1 << 30) return (b / (1 << 30)).toFixed(2) + " GB";
  if (b >= 1 << 20) return (b / (1 << 20)).toFixed(1) + " MB";
  if (b >= 1024) return (b / 1024).toFixed(0) + " KB";
  return b + " B";
}
function pct(a: number, b: number): string {
  return b === 0 ? "—" : ((a / b) * 100).toFixed(1) + "%";
}
function delta(cur: number, prev: number): { text: string; color: string } | null {
  if (prev === 0) return cur === 0 ? null : { text: "新增", color: "#64748b" };
  const d = ((cur - prev) / prev) * 100;
  const sign = d >= 0 ? "+" : "";
  return { text: `${sign}${d.toFixed(0)}% 环比`, color: d > 0 ? "#dc2626" : d < 0 ? "#16a34a" : "#64748b" };
}
function tsLabel(ts: string, range: RangeKey): string {
  const d = new Date(ts);
  if (range === "1h" || range === "24h") return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (range === "7d") return d.toLocaleDateString([], { month: "numeric", day: "numeric" }) + " " + d.toLocaleTimeString([], { hour: "2-digit" });
  return d.toLocaleDateString([], { month: "numeric", day: "numeric" });
}

interface Props {
  status: ProxyStatus | null;
}

export default function Monitor({ status }: Props) {
  const [range, setRange] = useState<RangeKey>("24h");
  const [report, setReport] = useState<UsageReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [live, setLive] = useState<RequestLogEvent[]>([]);
  const [pricingOpen, setPricingOpen] = useState(false);
  const [now, setNow] = useState(Date.now());
  const { message } = AntApp.useApp();
  const timer = useRef<number | null>(null);

  const load = async (r: RangeKey = range) => {
    setLoading(true);
    try {
      const cfg = RANGES[r];
      setReport(await api.usageReport(cfg.hours, cfg.bucket));
    } catch (e) {
      message.error(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load(range);
    const iv = setInterval(() => load(range), 30_000);
    return () => clearInterval(iv);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [range]);

  useEffect(() => {
    const tick = setInterval(() => setNow(Date.now()), 2000);
    const un = onRequest((log) => {
      setLive((l) => [log, ...l].slice(0, 200));
      // 事件后延迟刷新报表（合并连续事件）
      if (timer.current) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => load(), 1200);
    });
    return () => {
      clearInterval(tick);
      un.then((f) => f());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 最近 60 秒 / 5 分钟的实时窗口
  const liveStats = useMemo(() => {
    const cutoff60 = now - 60_000;
    const cutoff300 = now - 300_000;
    let r60 = 0, t60 = 0, r300 = 0, t300 = 0, tools60 = 0;
    for (const e of live) {
      const t = new Date(e.started_at).getTime() + e.duration_ms;
      const tok = (e.usage?.input_tokens ?? 0) + (e.usage?.output_tokens ?? 0);
      if (t >= cutoff300) {
        r300++;
        t300 += tok;
      }
      if (t >= cutoff60) {
        r60++;
        t60 += tok;
        tools60 += e.usage?.tool_calls.length ?? 0;
      }
    }
    return { r60, t60, r300, t300, tools60 };
  }, [live, now]);

  const series = useMemo(
    () =>
      (report?.series ?? []).map((b) => ({
        ...b,
        label: tsLabel(b.ts, range),
        uncached: Math.max(0, b.input_tokens - (report?.by_provider.some((p) => p.key === "openai") ? b.cache_read_tokens : 0)),
      })),
    [report, range],
  );

  const heatMax = useMemo(() => Math.max(1, ...(report?.heatmap.flat() ?? [0])), [report]);
  const t = report?.totals;
  const p = report?.previous;

  return (
    <div>
      <Space style={{ width: "100%", justifyContent: "space-between", marginBottom: 12 }} align="center">
        <h1 className="pg-page-title" style={{ margin: 0 }}>使用监控</h1>
        <Space>
          <Segmented<RangeKey>
            value={range}
            onChange={(v) => setRange(v)}
            options={(Object.keys(RANGES) as RangeKey[]).map((k) => ({ value: k, label: RANGES[k].label }))}
          />
          <Button icon={<ReloadOutlined />} loading={loading} onClick={() => load()} />
          <Button icon={<DollarOutlined />} onClick={() => setPricingOpen(true)}>定价</Button>
        </Space>
      </Space>

      {/* 实时条 */}
      <Card size="small" style={{ marginBottom: 12, background: "#0f172a" }} styles={{ body: { padding: "10px 16px" } }}>
        <Row gutter={24} align="middle">
          <Col>
            <Space>
              <span className={"dot" + (status?.running ? " on" : "")} />
              <Typography.Text style={{ color: "#e2e8f0" }}>{status?.running ? "代理运行中" : "代理已停止"}</Typography.Text>
            </Space>
          </Col>
          <LiveStat label="在途请求" value={status?.inflight ?? 0} highlight={(status?.inflight ?? 0) > 0} />
          <LiveStat label="最近 1 分钟" value={`${liveStats.r60} 次 · ${fmtNum(liveStats.t60)} tok`} />
          <LiveStat label="最近 5 分钟" value={`${liveStats.r300} 次 · ${fmtNum(liveStats.t300)} tok`} />
          <LiveStat label="1 分钟工具调用" value={liveStats.tools60} />
          <LiveStat label="本次启动累计" value={`${status?.total_requests ?? 0} 次`} />
          <Col flex="auto" style={{ textAlign: "right" }}>
            <Typography.Text style={{ color: "#64748b", fontSize: 12 }}>
              {live[0] ? `最近一次：${new Date(live[0].started_at).toLocaleTimeString()} ${live[0].agent} ${live[0].usage?.response_model ?? live[0].request_meta?.model ?? ""}` : "等待请求…"}
            </Typography.Text>
          </Col>
        </Row>
      </Card>

      {!report ? (
        <Card loading />
      ) : (
        <>
          {/* 概览指标 */}
          <Row gutter={[12, 12]}>
            <Kpi title="请求数" value={t!.requests} prev={p!.requests} extra={`模型调用 ${t!.model_calls} · 流式 ${pct(t!.streamed, t!.requests)}`} />
            <Kpi title="输入 tokens" value={t!.input_tokens} prev={p!.input_tokens} fmt={fmtNum} extra={`缓存读取 ${fmtNum(t!.cache_read_tokens)} · 写入 ${fmtNum(t!.cache_write_tokens)}`} />
            <Kpi title="输出 tokens" value={t!.output_tokens} prev={p!.output_tokens} fmt={fmtNum} extra={`含推理 ${fmtNum(t!.reasoning_tokens)}`} />
            <Kpi
              title="等效 API 费用"
              value={t!.cost_usd}
              prev={p!.cost_usd}
              fmt={fmtCost}
              extra={t!.priced_ratio < 1 ? `仅 ${pct(t!.priced_ratio, 1)} 的调用有定价` : "按定价表估算"}
              warn={t!.priced_ratio < 1}
            />
            <Kpi title="工具调用" value={t!.tool_calls} prev={p!.tool_calls} extra={`模型主动发起的工具 / 函数调用`} />
            <Kpi title="脱敏次数" value={t!.redactions} prev={p!.redactions} extra={`隐私替换命中`} />
            <Kpi title="错误" value={t!.errors} prev={p!.errors} extra={`错误率 ${pct(t!.errors, t!.requests)}`} invert />
            <Kpi
              title="会话数"
              value={report.session_count}
              prev={0}
              extra={`延迟 p50 ${fmtMs(report.latency.p50_ms)} · p95 ${fmtMs(report.latency.p95_ms)}`}
            />
          </Row>

          {/* 时间序列 */}
          <Row gutter={12} style={{ marginTop: 12 }}>
            <Col span={14}>
              <Card size="small" title="Token 用量走势">
                {series.length === 0 ? <Empty /> : (
                  <ResponsiveContainer width="100%" height={240}>
                    <AreaChart data={series} margin={{ left: 0, right: 8, top: 8, bottom: 0 }}>
                      <CartesianGrid strokeDasharray="3 3" stroke="#e5e7eb" />
                      <XAxis dataKey="label" tick={{ fontSize: 11 }} minTickGap={24} />
                      <YAxis tick={{ fontSize: 11 }} tickFormatter={fmtNum} width={48} />
                      <Tooltip formatter={(v) => fmtNum(Number(v))} />
                      <Legend wrapperStyle={{ fontSize: 12 }} />
                      <Area type="monotone" dataKey="uncached" name="输入（未缓存）" stackId="1" stroke="#2563eb" fill="#2563eb" fillOpacity={0.35} />
                      <Area type="monotone" dataKey="cache_read_tokens" name="缓存读取" stackId="1" stroke="#94a3b8" fill="#94a3b8" fillOpacity={0.3} />
                      <Area type="monotone" dataKey="output_tokens" name="输出" stackId="1" stroke="#16a34a" fill="#16a34a" fillOpacity={0.4} />
                      <Area type="monotone" dataKey="reasoning_tokens" name="推理" stackId="2" stroke="#7c3aed" fill="#7c3aed" fillOpacity={0.15} />
                    </AreaChart>
                  </ResponsiveContainer>
                )}
              </Card>
            </Col>
            <Col span={10}>
              <Card size="small" title="请求 / 错误 / 费用走势">
                {series.length === 0 ? <Empty /> : (
                  <ResponsiveContainer width="100%" height={240}>
                    <ComposedChart data={series} margin={{ left: 0, right: 8, top: 8, bottom: 0 }}>
                      <CartesianGrid strokeDasharray="3 3" stroke="#e5e7eb" />
                      <XAxis dataKey="label" tick={{ fontSize: 11 }} minTickGap={24} />
                      <YAxis yAxisId="l" tick={{ fontSize: 11 }} width={36} allowDecimals={false} />
                      <YAxis yAxisId="r" orientation="right" tick={{ fontSize: 11 }} width={48} tickFormatter={(v) => fmtCost(Number(v))} />
                      <Tooltip formatter={(v, name) => (name === "费用" ? fmtCost(Number(v)) : v)} />
                      <Legend wrapperStyle={{ fontSize: 12 }} />
                      <Bar yAxisId="l" dataKey="requests" name="请求" fill="#2563eb" stackId="a" />
                      <Bar yAxisId="l" dataKey="errors" name="错误" fill="#dc2626" stackId="b" />
                      <Line yAxisId="r" type="monotone" dataKey="cost_usd" name="费用" stroke="#f59e0b" dot={false} strokeWidth={2} />
                    </ComposedChart>
                  </ResponsiveContainer>
                )}
              </Card>
            </Col>
          </Row>

          {/* 模型表 */}
          <Card size="small" title="按模型" style={{ marginTop: 12 }}>
            <Table<ModelUsage>
              size="small"
              rowKey="model"
              pagination={false}
              dataSource={report.by_model}
              columns={[
                { title: "模型", dataIndex: "model", render: (m: string, r) => <Space size={4}><span className="mono">{m}</span><Tag>{r.provider}</Tag></Space> },
                { title: "调用", dataIndex: "requests", align: "right", sorter: (a, b) => a.requests - b.requests },
                { title: "输入", dataIndex: "input_tokens", align: "right", render: fmtNum, sorter: (a, b) => a.input_tokens - b.input_tokens },
                { title: "缓存读", dataIndex: "cache_read_tokens", align: "right", render: (v: number, r) => <span>{fmtNum(v)} <Typography.Text type="secondary" style={{ fontSize: 11 }}>{pct(r.cache_hit_rate, 1)}</Typography.Text></span> },
                { title: "输出", dataIndex: "output_tokens", align: "right", render: fmtNum, sorter: (a, b) => a.output_tokens - b.output_tokens },
                { title: "推理", dataIndex: "reasoning_tokens", align: "right", render: fmtNum },
                { title: "工具", dataIndex: "tool_calls", align: "right" },
                { title: "p50 / p95", align: "right", render: (_, r) => <span>{fmtMs(r.p50_duration_ms)} / {fmtMs(r.p95_duration_ms)}</span>, sorter: (a, b) => a.p50_duration_ms - b.p50_duration_ms },
                { title: "首字节", dataIndex: "p50_ttft_ms", align: "right", render: fmtMs },
                { title: "输出速率", dataIndex: "output_tps", align: "right", render: (v: number) => (v > 0 ? v.toFixed(0) + " tok/s" : "—") },
                { title: "错误", dataIndex: "errors", align: "right", render: (v: number) => (v ? <Tag color="red">{v}</Tag> : "0") },
                {
                  title: "费用", dataIndex: "cost_usd", align: "right", sorter: (a, b) => a.cost_usd - b.cost_usd,
                  render: (v: number, r) => (r.priced ? fmtCost(v) : <AntTooltip title="定价表中没有该模型，点右上角「定价」添加"><Typography.Text type="secondary">未定价</Typography.Text></AntTooltip>),
                },
              ]}
            />
          </Card>

          <Row gutter={12} style={{ marginTop: 12 }}>
            <Col span={8}>
              <Card size="small" title="按工具 / 提供方">
                <Row>
                  <Col span={12}>
                    <SharePie data={report.by_agent.map((g) => ({ name: AGENT_LABEL[g.key as keyof typeof AGENT_LABEL] ?? g.key, value: g.requests }))} title="客户端" />
                  </Col>
                  <Col span={12}>
                    <SharePie data={report.by_provider.map((g) => ({ name: g.key, value: g.requests }))} title="提供方" />
                  </Col>
                </Row>
                <Space wrap size={4} style={{ marginTop: 8 }}>
                  {report.by_api.map((g) => <Tag key={g.key}>{g.key}: {g.requests}</Tag>)}
                </Space>
              </Card>
            </Col>
            <Col span={8}>
              <Card size="small" title="模型调用的工具 Top 15">
                {report.tools.length === 0 ? <Empty description="窗口内没有工具调用" image={Empty.PRESENTED_IMAGE_SIMPLE} /> : (
                  <ResponsiveContainer width="100%" height={Math.max(120, Math.min(15, report.tools.length) * 22 + 20)}>
                    <BarChart layout="vertical" data={report.tools.slice(0, 15).map(([name, count]) => ({ name, count }))} margin={{ left: 8, right: 24, top: 0, bottom: 0 }}>
                      <XAxis type="number" hide />
                      <YAxis type="category" dataKey="name" width={110} tick={{ fontSize: 11 }} />
                      <Tooltip />
                      <Bar dataKey="count" name="次数" fill="#0891b2" radius={[0, 4, 4, 0]} label={{ position: "right", fontSize: 11 }} />
                    </BarChart>
                  </ResponsiveContainer>
                )}
              </Card>
            </Col>
            <Col span={8}>
              <Card size="small" title="结束原因 / 状态码 / 流量">
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>结束原因</Typography.Text>
                <div style={{ marginBottom: 8 }}>
                  <Space wrap size={4}>{report.stop_reasons.length ? report.stop_reasons.map(([k, v]) => <Tag key={k} color={k.includes("tool") || k === "completed" || k === "end_turn" || k === "stop" ? "green" : "orange"}>{k}: {v}</Tag>) : "—"}</Space>
                </div>
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>HTTP 状态</Typography.Text>
                <div style={{ marginBottom: 8 }}>
                  <Space wrap size={4}>{report.status_codes.length ? report.status_codes.map(([k, v]) => <Tag key={k} color={k.startsWith("2") ? "green" : k.startsWith("4") ? "orange" : "red"}>{k}: {v}</Tag>) : "—"}</Space>
                </div>
                <Row gutter={8}>
                  <Col span={12}><Statistic title="上行流量" value={fmtBytes(t!.req_bytes)} valueStyle={{ fontSize: 16 }} /></Col>
                  <Col span={12}><Statistic title="下行流量" value={fmtBytes(t!.resp_bytes)} valueStyle={{ fontSize: 16 }} /></Col>
                  <Col span={12}><Statistic title="p99 延迟" value={fmtMs(report.latency.p99_ms)} valueStyle={{ fontSize: 16 }} /></Col>
                  <Col span={12}><Statistic title="首字节 p50 / p95" value={`${fmtMs(report.latency.p50_ttft_ms)} / ${fmtMs(report.latency.p95_ttft_ms)}`} valueStyle={{ fontSize: 16 }} /></Col>
                </Row>
              </Card>
            </Col>
          </Row>

          {/* 时段分布 */}
          <Row gutter={12} style={{ marginTop: 12 }}>
            <Col span={10}>
              <Card size="small" title="一天中的使用时段（本地时间）">
                <ResponsiveContainer width="100%" height={160}>
                  <BarChart data={report.by_hour.map((v, h) => ({ h: `${h}`, v }))} margin={{ left: 0, right: 8, top: 8, bottom: 0 }}>
                    <XAxis dataKey="h" tick={{ fontSize: 10 }} interval={1} />
                    <YAxis tick={{ fontSize: 11 }} width={32} allowDecimals={false} />
                    <Tooltip labelFormatter={(l) => `${l}:00`} />
                    <Bar dataKey="v" name="请求" fill="#7c3aed" radius={[3, 3, 0, 0]} />
                  </BarChart>
                </ResponsiveContainer>
              </Card>
            </Col>
            <Col span={14}>
              <Card size="small" title="周 × 小时热力图">
                <div style={{ display: "grid", gridTemplateColumns: "28px repeat(24, 1fr)", gap: 2, fontSize: 10 }}>
                  <div />
                  {Array.from({ length: 24 }, (_, h) => <div key={h} style={{ textAlign: "center", color: "#94a3b8" }}>{h % 3 === 0 ? h : ""}</div>)}
                  {report.heatmap.map((row, wd) => (
                    <HeatRow key={wd} label={WEEKDAYS[wd]} row={row} max={heatMax} />
                  ))}
                </div>
              </Card>
            </Col>
          </Row>

          {/* 会话 */}
          <Card size="small" title={`会话（共 ${report.session_count} 个，显示最近 ${report.sessions.length} 个）`} style={{ marginTop: 12 }}>
            {report.sessions.length === 0 ? (
              <Empty description="没有识别到会话标识（Claude Code 的 metadata.user_id / Codex 的 session_id）" image={Empty.PRESENTED_IMAGE_SIMPLE} />
            ) : (
              <Table<SessionUsage>
                size="small"
                rowKey="session_id"
                pagination={{ pageSize: 10, size: "small" }}
                dataSource={report.sessions}
                columns={[
                  { title: "会话", dataIndex: "session_id", render: (s: string) => <span className="mono selectable" style={{ fontSize: 11 }}>{s.length > 20 ? s.slice(0, 8) + "…" + s.slice(-8) : s}</span> },
                  { title: "客户端", dataIndex: "agent", render: (a: string) => AGENT_LABEL[a as keyof typeof AGENT_LABEL] ?? a },
                  { title: "模型", dataIndex: "model", render: (m: string) => <span className="mono">{m}</span> },
                  { title: "开始", dataIndex: "first_seen", render: (v: string) => new Date(v).toLocaleString() },
                  { title: "最近", dataIndex: "last_seen", render: (v: string) => new Date(v).toLocaleTimeString() },
                  { title: "轮次", dataIndex: "requests", align: "right" },
                  { title: "输入 / 输出", align: "right", render: (_, r) => `${fmtNum(r.input_tokens)} / ${fmtNum(r.output_tokens)}` },
                  { title: "工具", dataIndex: "tool_calls", align: "right" },
                  { title: "脱敏", dataIndex: "redactions", align: "right", render: (v: number) => (v ? <Tag color="purple">{v}</Tag> : "0") },
                  { title: "费用", dataIndex: "cost_usd", align: "right", render: fmtCost },
                ]}
              />
            )}
          </Card>

          <Typography.Paragraph type="secondary" style={{ fontSize: 12, marginTop: 12 }}>
            费用为按定价表估算的「等效 API 费用」；ChatGPT / Claude 订阅登录的流量实际不按 token 计费。首字节 = 上游响应头（或 WebSocket 首个事件）到达的延迟。
            会话来自 Claude Code 的 <span className="mono">metadata.user_id</span>、Codex 的 <span className="mono">session_id</span> 请求头 / <span className="mono">prompt_cache_key</span>。
          </Typography.Paragraph>
        </>
      )}

      <PricingDrawer open={pricingOpen} onClose={() => { setPricingOpen(false); load(); }} />
    </div>
  );
}

function LiveStat({ label, value, highlight }: { label: string; value: string | number; highlight?: boolean }) {
  return (
    <Col>
      <div style={{ color: "#64748b", fontSize: 11 }}>{label}</div>
      <div style={{ color: highlight ? "#fbbf24" : "#f1f5f9", fontSize: 15, fontWeight: 600, fontVariantNumeric: "tabular-nums" }}>
        {highlight && <ThunderboltOutlined style={{ marginRight: 4 }} />}{value}
      </div>
    </Col>
  );
}

function Kpi({
  title, value, prev, fmt, extra, invert, warn,
}: { title: string; value: number; prev: number; fmt?: (n: number) => string; extra?: string; invert?: boolean; warn?: boolean }) {
  const d = delta(value, prev);
  const color = d ? (invert ? d.color : d.color === "#dc2626" ? "#16a34a" : d.color === "#16a34a" ? "#dc2626" : d.color) : undefined;
  return (
    <Col span={6}>
      <Card size="small">
        <Statistic title={title} value={fmt ? fmt(value) : value} valueStyle={{ fontSize: 22 }} />
        <div style={{ fontSize: 12, color: "#64748b", display: "flex", justifyContent: "space-between" }}>
          <span style={{ color: warn ? "#d97706" : undefined }}>{extra}</span>
          {d && <span style={{ color }}>{d.text}</span>}
        </div>
      </Card>
    </Col>
  );
}

function SharePie({ data, title }: { data: { name: string; value: number }[]; title: string }) {
  const total = data.reduce((s, d) => s + d.value, 0);
  return (
    <div style={{ textAlign: "center" }}>
      <Typography.Text type="secondary" style={{ fontSize: 12 }}>{title}</Typography.Text>
      {total === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={false} /> : (
        <ResponsiveContainer width="100%" height={150}>
          <PieChart>
            <Pie data={data} dataKey="value" nameKey="name" innerRadius={32} outerRadius={55} paddingAngle={2}>
              {data.map((_, i) => <Cell key={i} fill={COLORS[i % COLORS.length]} />)}
            </Pie>
            <Tooltip formatter={(v, n) => [`${v} (${pct(Number(v), total)})`, n]} />
            <Legend wrapperStyle={{ fontSize: 11 }} iconSize={8} />
          </PieChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}

function HeatRow({ label, row, max }: { label: string; row: number[]; max: number }) {
  return (
    <>
      <div style={{ color: "#64748b", lineHeight: "16px" }}>{label}</div>
      {row.map((v, h) => (
        <AntTooltip key={h} title={`周${label} ${h}:00 — ${v} 次`}>
          <div style={{ height: 16, borderRadius: 2, background: v === 0 ? "#f1f5f9" : `rgba(37, 99, 235, ${0.15 + 0.85 * (v / max)})` }} />
        </AntTooltip>
      ))}
    </>
  );
}

function PricingDrawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [rows, setRows] = useState<ModelPrice[]>([]);
  const [customized, setCustomized] = useState(false);
  const [busy, setBusy] = useState(false);
  const { message } = AntApp.useApp();

  useEffect(() => {
    if (open) {
      api.getPricing().then((v) => { setRows(v.prices); setCustomized(v.customized); }).catch((e) => message.error(String(e)));
    }
  }, [open]);

  const update = (i: number, patch: Partial<ModelPrice>) => setRows((r) => r.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const save = async (prices: ModelPrice[]) => {
    setBusy(true);
    try {
      const v = await api.setPricing(prices);
      setRows(v.prices);
      setCustomized(v.customized);
      message.success(prices.length ? "定价已保存" : "已恢复内置参考价");
    } catch (e) {
      message.error(String(e));
    } finally {
      setBusy(false);
    }
  };

  const num = (i: number, key: keyof ModelPrice) => (
    <InputNumber size="small" min={0} step={0.05} style={{ width: 84 }} value={rows[i][key] as number} onChange={(v) => update(i, { [key]: v ?? 0 } as Partial<ModelPrice>)} />
  );

  return (
    <Drawer title="模型定价（USD / 百万 tokens）" open={open} onClose={onClose} width={720}
      extra={
        <Space>
          <Popconfirm title="恢复内置参考价？" onConfirm={() => save([])}><Button disabled={!customized || busy}>恢复默认</Button></Popconfirm>
          <Button type="primary" loading={busy} onClick={() => save(rows)}>保存</Button>
        </Space>
      }
    >
      <Alert type="info" showIcon style={{ marginBottom: 12 }} message="按模型名前缀匹配（不区分大小写），最长前缀优先。内置表只是参考价，请按你实际的账单价格修改。" />
      <Table<ModelPrice>
        size="small"
        rowKey={(_, i) => String(i)}
        pagination={false}
        dataSource={rows}
        columns={[
          { title: "模型前缀", render: (_, __, i) => <Input size="small" className="mono" value={rows[i].model_prefix} onChange={(e) => update(i, { model_prefix: e.target.value })} /> },
          { title: "输入", width: 100, render: (_, __, i) => num(i, "input_per_m") },
          { title: "输出", width: 100, render: (_, __, i) => num(i, "output_per_m") },
          { title: "缓存读", width: 100, render: (_, __, i) => num(i, "cache_read_per_m") },
          { title: "缓存写", width: 100, render: (_, __, i) => num(i, "cache_write_per_m") },
          { title: "", width: 40, render: (_, __, i) => <Button size="small" type="text" danger icon={<DeleteOutlined />} onClick={() => setRows((r) => r.filter((_, j) => j !== i))} /> },
        ]}
        footer={() => (
          <Button size="small" icon={<PlusOutlined />} onClick={() => setRows((r) => [...r, { model_prefix: "", input_per_m: 0, output_per_m: 0, cache_read_per_m: 0, cache_write_per_m: 0 }])}>添加</Button>
        )}
      />
    </Drawer>
  );
}

export type { UsageTotals };
