import { useEffect, useState } from "react";
import { Button, Card, Descriptions, Drawer, Input, Popconfirm, Select, Space, Switch, Table, Tag, Typography, Alert, App as AntApp } from "antd";
import { ReloadOutlined } from "@ant-design/icons";
import { api, onRequest, type RequestDetail, type RequestFilter, type RequestRow } from "../api";

export default function Logs() {
  const [rows, setRows] = useState<RequestRow[]>([]);
  const [filter, setFilter] = useState<RequestFilter>({ only_redacted: false, limit: 200, offset: 0 });
  const [detail, setDetail] = useState<RequestDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [live, setLive] = useState(true);
  const { message } = AntApp.useApp();

  const load = async (f = filter) => {
    setLoading(true);
    try {
      setRows(await api.listRequests(f));
    } catch (e) {
      message.error(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load(filter);
  }, [filter]);

  useEffect(() => {
    if (!live) return;
    const un = onRequest(() => load());
    return () => {
      un.then((f) => f());
    };
  }, [live, filter]);

  const openDetail = async (id: string) => {
    try {
      setDetail(await api.requestDetail(id));
    } catch (e) {
      message.error(String(e));
    }
  };

  return (
    <div>
      <h1 className="pg-page-title">请求与模型调用记录</h1>
      <Card
        size="small"
        style={{ marginBottom: 12 }}
        title={
          <Space wrap>
            <Select
              allowClear
              placeholder="工具"
              style={{ width: 120 }}
              value={filter.agent ?? undefined}
              onChange={(v) => setFilter({ ...filter, agent: v ?? null })}
              options={[{ value: "claude", label: "Claude Code" }, { value: "codex", label: "Codex" }, { value: "unknown", label: "其他" }]}
            />
            <Select
              allowClear
              placeholder="提供商"
              style={{ width: 140 }}
              value={filter.provider ?? undefined}
              onChange={(v) => setFilter({ ...filter, provider: v ?? null })}
              options={[{ value: "anthropic", label: "Anthropic" }, { value: "openai", label: "OpenAI" }, { value: "chatgpt_codex", label: "ChatGPT (Codex)" }]}
            />
            <Select
              allowClear
              placeholder="结果"
              style={{ width: 110 }}
              value={filter.outcome ?? undefined}
              onChange={(v) => setFilter({ ...filter, outcome: v ?? null })}
              options={[{ value: "ok", label: "成功" }, { value: "error", label: "失败" }]}
            />
            <Input.Search
              allowClear
              placeholder="路径 / 模型"
              style={{ width: 200 }}
              onSearch={(v) => setFilter({ ...filter, search: v || null })}
            />
            <span>
              仅脱敏 <Switch size="small" checked={filter.only_redacted} onChange={(v) => setFilter({ ...filter, only_redacted: v })} />
            </span>
          </Space>
        }
        extra={
          <Space>
            <span>
              实时 <Switch size="small" checked={live} onChange={setLive} />
            </span>
            <Button size="small" icon={<ReloadOutlined />} onClick={() => load()} />
            <Popconfirm title="清空全部日志？" onConfirm={async () => { await api.clearLogs(); load(); }}>
              <Button size="small" danger>清空</Button>
            </Popconfirm>
          </Space>
        }
      >
        <Table
          size="small"
          rowKey="id"
          loading={loading}
          dataSource={rows}
          pagination={{ pageSize: 50, showSizeChanger: false }}
          onRow={(r) => ({ onClick: () => openDetail(r.id), style: { cursor: "pointer" } })}
          columns={[
            { title: "时间", dataIndex: "started_at", width: 150, render: (t) => <span style={{ fontSize: 12 }}>{new Date(t).toLocaleString()}</span> },
            { title: "工具", dataIndex: "agent", width: 80 },
            {
              title: "状态", dataIndex: "status", width: 70,
              render: (s, r) => <Tag color={r.error || (s ?? 0) >= 400 ? "red" : s ? "green" : "default"}>{s ?? "—"}</Tag>,
            },
            { title: "接口", render: (_, r) => <span className="mono" style={{ fontSize: 12 }}>{r.method} {r.host}{r.path}</span>, ellipsis: true },
            { title: "模型", dataIndex: "model", width: 190, ellipsis: true, render: (m) => m ? <span className="mono" style={{ fontSize: 12 }}>{m}</span> : <span style={{ color: "#cbd5e1" }}>—</span> },
            { title: "脱敏", dataIndex: "redaction_count", width: 70, render: (n) => (n > 0 ? <Tag color="blue">{n}</Tag> : <span style={{ color: "#cbd5e1" }}>0</span>) },
            { title: "还原", dataIndex: "restored_count", width: 70, render: (n) => (n > 0 ? <Tag color="cyan">{n}</Tag> : <span style={{ color: "#cbd5e1" }}>0</span>) },
            { title: "tokens", width: 120, render: (_, r) => r.input_tokens != null ? <span style={{ fontSize: 12 }}>{r.input_tokens} / {r.output_tokens ?? "?"}</span> : null },
            { title: "耗时", dataIndex: "duration_ms", width: 80, render: (ms) => <span style={{ fontSize: 12 }}>{ms} ms</span> },
          ]}
        />
      </Card>

      <Drawer title="请求详情" open={!!detail} onClose={() => setDetail(null)} width={640}>
        {detail && (
          <>
            {detail.request.error && <Alert type="error" showIcon message={detail.request.error} style={{ marginBottom: 12 }} />}
            {detail.warnings.map((w, i) => <Alert key={i} type="warning" showIcon message={w} style={{ marginBottom: 12 }} />)}
            <Descriptions title="请求" size="small" column={2} bordered>
              <Descriptions.Item label="ID" span={2}><span className="mono selectable" style={{ fontSize: 11 }}>{detail.request.id}</span></Descriptions.Item>
              <Descriptions.Item label="时间">{new Date(detail.request.started_at).toLocaleString()}</Descriptions.Item>
              <Descriptions.Item label="耗时">{detail.request.duration_ms} ms{detail.request.ttft_ms != null ? `（首字节 ${detail.request.ttft_ms} ms）` : ""}</Descriptions.Item>
              <Descriptions.Item label="入口">{detail.request.entry}</Descriptions.Item>
              <Descriptions.Item label="工具">{detail.request.agent}</Descriptions.Item>
              <Descriptions.Item label="接口" span={2}><span className="mono selectable">{detail.request.method} {detail.request.host}{detail.request.path}</span></Descriptions.Item>
              <Descriptions.Item label="状态">{detail.request.status ?? "—"}</Descriptions.Item>
              <Descriptions.Item label="流式">{detail.request.streamed ? "是" : "否"}</Descriptions.Item>
              <Descriptions.Item label="请求体">{fmtBytes(detail.request.req_bytes)}</Descriptions.Item>
              <Descriptions.Item label="响应体">{fmtBytes(detail.request.resp_bytes)}</Descriptions.Item>
              <Descriptions.Item label="会话" span={2}><span className="mono selectable" style={{ fontSize: 11 }}>{detail.request.session_id ?? "—"}</span></Descriptions.Item>
            </Descriptions>

            {detail.model_call && (
              <Descriptions title="模型调用" size="small" column={2} bordered style={{ marginTop: 16 }}>
                <Descriptions.Item label="请求模型"><span className="mono">{detail.model_call.model ?? "—"}</span></Descriptions.Item>
                <Descriptions.Item label="响应模型"><span className="mono">{detail.model_call.response_model ?? "—"}</span></Descriptions.Item>
                <Descriptions.Item label="输入 tokens">{detail.model_call.input_tokens ?? "—"}</Descriptions.Item>
                <Descriptions.Item label="输出 tokens">{detail.model_call.output_tokens ?? "—"}</Descriptions.Item>
                <Descriptions.Item label="缓存读取">{detail.model_call.cache_read_tokens ?? "—"}</Descriptions.Item>
                <Descriptions.Item label="缓存写入">{detail.model_call.cache_write_tokens ?? "—"}</Descriptions.Item>
                <Descriptions.Item label="消息数">{detail.model_call.message_count}</Descriptions.Item>
                <Descriptions.Item label="结束原因">{detail.model_call.stop_reason ?? "—"}</Descriptions.Item>
                <Descriptions.Item label="推理 tokens">{detail.model_call.reasoning_tokens ?? "—"}{detail.model_call.reasoning_effort ? `（${detail.model_call.reasoning_effort}）` : ""}</Descriptions.Item>
                <Descriptions.Item label="模型调用的工具" span={2}>
                  <Space wrap size={4}>
                    {detail.model_call.tool_calls.length ? detail.model_call.tool_calls.map((t, i) => <Tag key={t + i} color="geekblue">{t}</Tag>) : "—"}
                  </Space>
                </Descriptions.Item>
                <Descriptions.Item label="可用工具" span={2}>
                  <Space wrap size={4}>
                    {detail.model_call.tool_names.length ? detail.model_call.tool_names.map((t) => <Tag key={t}>{t}</Tag>) : "—"}
                  </Space>
                </Descriptions.Item>
              </Descriptions>
            )}

            <Typography.Title level={5} style={{ marginTop: 16 }}>脱敏明细</Typography.Title>
            {detail.redactions.length === 0 ? (
              <Typography.Text type="secondary">本次请求未命中任何规则</Typography.Text>
            ) : (
              <Table
                size="small"
                pagination={false}
                rowKey={(r) => r.rule_id + r.entity_type}
                dataSource={detail.redactions}
                columns={[
                  { title: "规则", dataIndex: "rule_id", render: (v) => <span className="mono">{v}</span> },
                  { title: "实体", dataIndex: "entity_type", render: (v) => <Tag color="blue">{v}</Tag> },
                  { title: "次数", dataIndex: "count", width: 80 },
                ]}
              />
            )}

            {detail.redacted_body && (
              <>
                <Typography.Title level={5} style={{ marginTop: 16 }}>脱敏后的请求体（调试模式）</Typography.Title>
                <pre className="pem selectable" style={{ maxHeight: 360, whiteSpace: "pre-wrap" }}>{pretty(detail.redacted_body)}</pre>
              </>
            )}
          </>
        )}
      </Drawer>
    </div>
  );
}

function fmtBytes(n: number) {
  if (n > 1024 * 1024) return (n / 1024 / 1024).toFixed(2) + " MB";
  if (n > 1024) return (n / 1024).toFixed(1) + " KB";
  return n + " B";
}

function pretty(s: string) {
  try {
    return JSON.stringify(JSON.parse(s), null, 2);
  } catch {
    return s;
  }
}
