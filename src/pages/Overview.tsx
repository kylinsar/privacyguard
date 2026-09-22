import { useEffect, useState } from "react";
import { Alert, Button, Card, Col, Row, Space, Statistic, Tag, Typography, App as AntApp, List } from "antd";
import { api, onRequest, type ProxyStatus, type Stats, type RequestLogEvent, type AgentView, AGENT_LABEL, MODE_LABEL } from "../api";

interface Props {
  status: ProxyStatus | null;
  onStatus: (s: ProxyStatus) => void;
  goto: (page: string) => void;
}

export default function Overview({ status, onStatus, goto }: Props) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [recent, setRecent] = useState<RequestLogEvent[]>([]);
  const [agents, setAgents] = useState<AgentView[]>([]);
  const [busy, setBusy] = useState(false);
  const { message } = AntApp.useApp();

  const load = () => {
    api.stats(24).then(setStats).catch(() => {});
    Promise.all([api.agentView("claude"), api.agentView("codex")]).then(setAgents).catch(() => {});
  };

  useEffect(() => {
    load();
    const un = onRequest((log) => {
      setRecent((r) => [log, ...r].slice(0, 8));
      load();
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const toggle = async () => {
    setBusy(true);
    try {
      onStatus(status?.running ? await api.proxyStop() : await api.proxyStart());
    } catch (e) {
      message.error(String(e));
    } finally {
      setBusy(false);
    }
  };

  const enabledAgents = agents.filter((a) => a.activation.enabled);
  const warnings = agents.filter((a) => a.activation.enabled).flatMap((a) => a.notices).slice(0, 4);

  return (
    <div>
      <h1 className="pg-page-title">概览</h1>
      {status?.last_error && <Alert type="error" showIcon message="代理启动失败" description={status.last_error} style={{ marginBottom: 16 }} />}
      <Row gutter={16}>
        <Col span={8}>
          <Card>
            <Space direction="vertical" style={{ width: "100%" }}>
              <Statistic title="代理状态" value={status?.running ? "运行中" : "已停止"} valueStyle={{ color: status?.running ? "#16a34a" : "#94a3b8" }} />
              <div className="mono" style={{ fontSize: 12, color: "#64748b" }}>{status?.proxy_url ?? "—"}</div>
              <Button type={status?.running ? "default" : "primary"} loading={busy} onClick={toggle}>
                {status?.running ? "停止代理" : "启动代理"}
              </Button>
            </Space>
          </Card>
        </Col>
        <Col span={8}>
          <Card>
            <Statistic title="24 小时请求" value={stats?.total_requests ?? 0} />
            <div style={{ marginTop: 8, color: "#64748b", fontSize: 12 }}>
              错误 {stats?.total_errors ?? 0} · 输入 {fmt(stats?.total_input_tokens)} / 输出 {fmt(stats?.total_output_tokens)} tokens
            </div>
          </Card>
        </Col>
        <Col span={8}>
          <Card>
            <Statistic title="24 小时脱敏次数" value={stats?.total_redactions ?? 0} valueStyle={{ color: "#1e40af" }} />
            <div style={{ marginTop: 8 }}>
              {(stats?.by_entity ?? []).slice(0, 5).map(([k, v]) => (
                <Tag key={k}>{k} × {v}</Tag>
              ))}
              {!stats?.by_entity?.length && <span style={{ color: "#94a3b8", fontSize: 12 }}>暂无</span>}
            </div>
          </Card>
        </Col>
      </Row>

      <Row gutter={16} style={{ marginTop: 16 }}>
        <Col span={12}>
          <Card title="保护状态" extra={<a onClick={() => goto("protection")}>管理</a>}>
            {agents.map((a) => (
              <div key={a.info.agent} style={{ display: "flex", justifyContent: "space-between", padding: "6px 0" }}>
                <span>
                  {AGENT_LABEL[a.info.agent]}{" "}
                  <span style={{ color: "#94a3b8", fontSize: 12 }}>{a.info.version ?? (a.info.installed ? "" : "未安装")}</span>
                </span>
                <span>
                  {a.activation.enabled ? (
                    <Tag color="green">{a.activation.mode ? MODE_LABEL[a.activation.mode] : "已开启"}</Tag>
                  ) : (
                    <Tag>未保护</Tag>
                  )}
                </span>
              </div>
            ))}
            {enabledAgents.length === 0 && (
              <Alert type="warning" showIcon style={{ marginTop: 8 }} message="尚未为任何编码代理开启保护" />
            )}
            {warnings.map((w, i) => (
              <Alert key={i} type="info" showIcon style={{ marginTop: 8 }} message={w} />
            ))}
          </Card>
        </Col>
        <Col span={12}>
          <Card title="最近请求" extra={<a onClick={() => goto("logs")}>全部</a>}>
            <List
              size="small"
              dataSource={recent}
              locale={{ emptyText: "等待请求…" }}
              renderItem={(r) => (
                <List.Item>
                  <Space size={6} wrap>
                    <Tag color={r.error || (r.status ?? 0) >= 400 ? "red" : "green"}>{r.status ?? "—"}</Tag>
                    <span>{r.agent}</span>
                    <span className="mono" style={{ fontSize: 12 }}>{r.host}{r.path}</span>
                    {r.redaction_events.length > 0 && (
                      <Tag color="blue">脱敏 {r.redaction_events.reduce((s, e) => s + e.count, 0)}</Tag>
                    )}
                    <span style={{ color: "#94a3b8", fontSize: 12 }}>{r.duration_ms} ms</span>
                  </Space>
                </List.Item>
              )}
            />
          </Card>
        </Col>
      </Row>
      <Typography.Paragraph type="secondary" style={{ marginTop: 16, fontSize: 12 }}>
        所有检测与脱敏均在本机完成；日志不保存任何原始隐私值，仅记录命中的规则与次数。
      </Typography.Paragraph>
    </div>
  );
}

function fmt(n?: number) {
  if (!n) return "0";
  if (n > 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n > 1000) return (n / 1000).toFixed(1) + "k";
  return String(n);
}
