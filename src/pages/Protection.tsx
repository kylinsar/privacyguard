import { useEffect, useState } from "react";
import { Alert, Button, Card, Col, Descriptions, Radio, Row, Space, Switch, Tag, Typography, App as AntApp, Collapse, Table } from "antd";
import {
  api,
  AGENT_LABEL,
  MODE_LABEL,
  type Agent,
  type AgentView,
  type PacStatus,
  type ProtectionMode,
  type ProxyStatus,
} from "../api";

interface Props {
  status: ProxyStatus | null;
  onStatus: (s: ProxyStatus) => void;
}

const MODE_HELP: Record<ProtectionMode, Record<Agent, string>> = {
  config: {
    claude: "写入 ~/.claude/settings.json 的 env（HTTPS_PROXY、NODE_EXTRA_CA_CERTS）。任何终端 / IDE 中启动的 claude 均受保护；关闭时精确回滚。",
    codex: "写入 ~/.codex/config.toml 的 base URL 覆盖，指向本地反向入口（不经 TLS 拦截）：ChatGPT 登录只写 chatgpt_base_url，API Key 登录另写 openai_base_url。Codex 的 WebSocket 与 zstd 压缩请求均会被解开脱敏。",
  },
  system_proxy: {
    claude: "系统 PAC 只对 api.anthropic.com 生效；由于 Claude Code 不读系统代理，会额外写入 settings.json 的 HTTPS_PROXY。需要 CA 已被系统信任并授权一次管理员权限。",
    codex: "系统 PAC 对 api.openai.com / chatgpt.com 生效，Codex 会自动解析系统代理并信任钥匙串中的 CA。不修改 Codex 任何配置。需要管理员授权一次。",
  },
  launcher: {
    claude: "不改任何配置。使用 pg claude（或 pg-claude）启动时注入代理与 CA 环境变量；直接运行 claude 不受保护。",
    codex: "不改任何配置。使用 pg codex（或 pg-codex）启动时注入 HTTPS_PROXY 与 CODEX_CA_CERTIFICATE；直接运行 codex 不受保护。",
  },
};

export default function Protection({ status, onStatus }: Props) {
  const [views, setViews] = useState<Record<Agent, AgentView | null>>({ claude: null, codex: null });
  const [pac, setPac] = useState<PacStatus | null>(null);
  const [mode, setMode] = useState<Record<Agent, ProtectionMode>>({ claude: "config", codex: "config" });
  const [busy, setBusy] = useState<Agent | "pac" | null>(null);
  const { message, modal } = AntApp.useApp();

  const load = async () => {
    const [c, x, p] = await Promise.all([api.agentView("claude"), api.agentView("codex"), api.pacStatus().catch(() => null)]);
    setViews({ claude: c, codex: x });
    setPac(p);
    setMode((m) => ({
      claude: c.activation.mode ?? m.claude,
      codex: x.activation.mode ?? m.codex,
    }));
  };

  useEffect(() => {
    load().catch((e) => message.error(String(e)));
  }, []);

  const toggle = async (agent: Agent, on: boolean) => {
    setBusy(agent);
    try {
      if (on) {
        const m = mode[agent];
        if (m === "system_proxy") {
          const ok = await new Promise<boolean>((resolve) =>
            modal.confirm({
              title: "启用系统代理 (PAC)",
              content: "将修改所有网络服务的「自动代理配置」，只有 OpenAI / Anthropic 域名走本地代理，其余直连。需要输入管理员密码。应用退出后 PAC 仍会保留（有 DIRECT 回退），下次启动自动恢复保护。",
              onOk: () => resolve(true),
              onCancel: () => resolve(false),
            }),
          );
          if (!ok) return;
        }
        await api.agentEnable(agent, m);
        message.success(`${AGENT_LABEL[agent]} 已开启保护（${MODE_LABEL[m]}）`);
      } else {
        await api.agentDisable(agent);
        message.success(`${AGENT_LABEL[agent]} 已关闭保护并回滚配置`);
      }
      onStatus(await api.proxyStatus());
      await load();
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(null);
    }
  };

  const changeMode = async (agent: Agent, m: ProtectionMode) => {
    setMode((s) => ({ ...s, [agent]: m }));
    const v = views[agent];
    if (v?.activation.enabled && v.activation.mode !== m) {
      setBusy(agent);
      try {
        await api.agentEnable(agent, m);
        message.success(`已切换为 ${MODE_LABEL[m]}`);
        await load();
      } catch (e) {
        message.error(String(e), 6);
        await load();
      } finally {
        setBusy(null);
      }
    }
  };

  return (
    <div>
      <h1 className="pg-page-title">保护</h1>
      {!status?.running && (
        <Alert type="warning" showIcon style={{ marginBottom: 16 }} message="代理未运行。开启任一保护时会自动启动代理；也可在概览页手动启动。" />
      )}
      <Row gutter={16}>
        {(["claude", "codex"] as Agent[]).map((agent) => {
          const v = views[agent];
          const enabled = !!v?.activation.enabled;
          return (
            <Col span={12} key={agent}>
              <Card
                title={
                  <Space>
                    {AGENT_LABEL[agent]}
                    {v?.info.installed ? <Tag color="blue">{v.info.version || "已安装"}</Tag> : <Tag>未检测到</Tag>}
                    {agent === "codex" && v?.info.auth_mode && <Tag>登录: {v.info.auth_mode}</Tag>}
                  </Space>
                }
                extra={
                  <Switch
                    checked={enabled}
                    loading={busy === agent}
                    onChange={(on) => toggle(agent, on)}
                    checkedChildren="保护中"
                    unCheckedChildren="关闭"
                  />
                }
              >
                <Radio.Group
                  value={mode[agent]}
                  onChange={(e) => changeMode(agent, e.target.value)}
                  disabled={busy === agent}
                  style={{ display: "flex", flexDirection: "column", gap: 8 }}
                >
                  {(["config", "system_proxy", "launcher"] as ProtectionMode[]).map((m) => (
                    <Radio key={m} value={m}>
                      <b>{MODE_LABEL[m]}</b>
                      <div style={{ color: "#64748b", fontSize: 12, marginTop: 2 }}>{MODE_HELP[m][agent]}</div>
                    </Radio>
                  ))}
                </Radio.Group>
                <Descriptions size="small" column={1} style={{ marginTop: 16 }}>
                  <Descriptions.Item label="可执行文件">
                    <span className="mono selectable" style={{ fontSize: 12 }}>{v?.info.binary ?? "—"}</span>
                  </Descriptions.Item>
                  <Descriptions.Item label="配置文件">
                    <span className="mono selectable" style={{ fontSize: 12 }}>
                      {v?.info.config_path} {v?.info.config_managed_by_us && <Tag color="green">已接管</Tag>}
                    </span>
                  </Descriptions.Item>
                  {v?.activation.activated_at && (
                    <Descriptions.Item label="开启时间">{new Date(v.activation.activated_at).toLocaleString()}</Descriptions.Item>
                  )}
                </Descriptions>
                {v?.notices.map((n, i) => (
                  <Alert key={i} type={n.includes("冲突") || n.includes("未指向") ? "warning" : "info"} showIcon message={n} style={{ marginTop: 8 }} />
                ))}
                {mode[agent] === "launcher" && (
                  <Alert
                    type="success"
                    style={{ marginTop: 8 }}
                    message={
                      <span>
                        用法：<code>pg {agent} …</code> 或 <code>pg-{agent} …</code>。在「设置」页安装命令行工具。
                      </span>
                    }
                  />
                )}
              </Card>
            </Col>
          );
        })}
      </Row>

      <Card title="系统代理 (PAC) 状态" style={{ marginTop: 16 }}
        extra={
          <Space>
            <Button size="small" loading={busy === "pac"} disabled={!pac?.hosts.length} onClick={async () => {
              setBusy("pac");
              try { setPac(await api.pacReapply()); message.success("已重新应用 PAC"); } catch (e) { message.error(String(e)); } finally { setBusy(null); }
            }}>重新应用</Button>
            <Button size="small" danger loading={busy === "pac"} disabled={!pac?.managed} onClick={async () => {
              setBusy("pac");
              try { setPac(await api.pacRestore()); message.success("已恢复系统原始代理设置"); await load(); } catch (e) { message.error(String(e)); } finally { setBusy(null); }
            }}>恢复原始设置</Button>
          </Space>
        }>
        <Space wrap style={{ marginBottom: 8 }}>
          <Tag color={pac?.managed ? "blue" : "default"}>{pac?.managed ? "由 PrivacyGuard 管理" : "未接管"}</Tag>
          <Tag color={pac?.active ? "green" : "default"}>{pac?.active ? "系统正在使用我们的 PAC" : "系统未指向我们的 PAC"}</Tag>
          <span className="mono" style={{ fontSize: 12 }}>{pac?.pac_url}</span>
        </Space>
        <div style={{ fontSize: 12, color: "#64748b", marginBottom: 8 }}>
          PAC 域名：{pac?.hosts.length ? pac.hosts.join(", ") : "（无工具使用模式 B）"}
        </div>
        <Collapse
          size="small"
          items={[{
            key: "svc",
            label: "各网络服务当前自动代理设置",
            children: (
              <Table
                size="small"
                pagination={false}
                rowKey="service"
                dataSource={pac?.services ?? []}
                columns={[
                  { title: "服务", dataIndex: "service" },
                  { title: "PAC URL", dataIndex: "url", render: (u) => <span className="mono">{u ?? "—"}</span> },
                  { title: "启用", dataIndex: "enabled", render: (e) => (e ? <Tag color="green">是</Tag> : <Tag>否</Tag>) },
                ]}
              />
            ),
          }]}
        />
      </Card>
      <Typography.Paragraph type="secondary" style={{ marginTop: 16, fontSize: 12 }}>
        三种模式可按工具独立选择。A 影响面最小、无需管理员权限；B 覆盖所有走系统代理的进程（含浏览器）但需要系统证书与管理员授权；C 零副作用但需要记得用 pg 启动。
      </Typography.Paragraph>
    </div>
  );
}
