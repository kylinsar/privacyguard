import { useEffect, useState } from "react";
import { Alert, Button, Card, Descriptions, Form, Input, InputNumber, Select, Space, Switch, Tag, Typography, App as AntApp } from "antd";
import { enable as autostartEnable, disable as autostartDisable, isEnabled as autostartIsEnabled } from "@tauri-apps/plugin-autostart";
import { api, type AppSettings, type CliStatus, type Paths, type ProxyStatus } from "../api";

interface Props {
  onStatus: (s: ProxyStatus) => void;
}

export default function Settings({ onStatus }: Props) {
  const [form] = Form.useForm<AppSettings>();
  const [paths, setPaths] = useState<Paths | null>(null);
  const [cli, setCli] = useState<CliStatus | null>(null);
  const [cliError, setCliError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const { message } = AntApp.useApp();

  const load = async () => {
    const [s, p] = await Promise.all([api.getSettings(), api.appPaths()]);
    form.setFieldsValue(s);
    setPaths(p);
    try {
      setCli(await api.cliStatus());
      setCliError(null);
    } catch (e) {
      setCliError(String(e));
    }
  };

  useEffect(() => {
    load().catch((e) => message.error(String(e)));
  }, []);

  const save = async () => {
    const values = await form.validateFields();
    setBusy("save");
    try {
      // 以库中最新设置为底，避免本页未展示的字段（如隐身身份）被重置
      const current = await api.getSettings();
      const st = await api.saveSettings({
        ...current,
        ...values,
        upstream_proxy: values.upstream_proxy?.trim() ? values.upstream_proxy.trim() : null,
      });
      onStatus(st);
      try {
        if (values.autostart) await autostartEnable();
        else if (await autostartIsEnabled()) await autostartDisable();
      } catch (e) {
        message.warning("开机自启设置失败: " + String(e));
      }
      message.success("已保存");
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(null);
    }
  };

  const cliAction = async (f: () => Promise<CliStatus>, ok: string) => {
    setBusy("cli");
    try {
      setCli(await f());
      message.success(ok);
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div>
      <h1 className="pg-page-title">设置</h1>
      <Card title="代理" extra={<Button type="primary" loading={busy === "save"} onClick={save}>保存</Button>}>
        <Form form={form} layout="vertical">
          <Space size={24} align="start" wrap>
            <Form.Item name="port" label="监听端口（127.0.0.1）" rules={[{ required: true }]} extra="模式 A/B 依赖固定端口；修改后需重新开启保护以更新配置">
              <InputNumber min={1024} max={65535} />
            </Form.Item>
            <Form.Item name="log_retention_days" label="日志保留天数" extra="0 表示永久保留">
              <InputNumber min={0} max={3650} />
            </Form.Item>
          </Space>
          <Form.Item name="intercept_hosts" label="拦截并脱敏的域名" extra="其他域名的 HTTPS 流量原样隧道，不解密">
            <Select mode="tags" tokenSeparators={[",", " "]} />
          </Form.Item>
          <Form.Item name="upstream_proxy" label="上游代理（可选）" extra="如需经企业代理 / 翻墙工具访问 OpenAI，填入 http://127.0.0.1:7890 之类；留空则直连（并忽略环境变量，避免回环）">
            <Input className="mono" placeholder="http://host:port 或 socks5://host:port" allowClear />
          </Form.Item>
          <Space size={32} wrap>
            <Form.Item
              name="restore_responses"
              label="还原模型输出中的占位符"
              valuePropName="checked"
              extra="默认开启：模型只看到 PG_xxx 占位符，代理在回复中换回原值。关闭后回复里会直接显示占位符，便于确认模型实际拿到的内容"
            >
              <Switch />
            </Form.Item>
            <Form.Item name="store_redacted_bodies" label="调试：保存脱敏后的请求体" valuePropName="checked" extra="仅保存脱敏后的内容，用于核对规则效果">
              <Switch />
            </Form.Item>
            <Form.Item name="start_proxy_on_launch" label="启动应用时自动运行代理" valuePropName="checked">
              <Switch />
            </Form.Item>
            <Form.Item name="autostart" label="开机自启" valuePropName="checked">
              <Switch />
            </Form.Item>
            <Form.Item name="minimize_to_tray" label="关闭窗口时最小化到托盘" valuePropName="checked">
              <Switch />
            </Form.Item>
          </Space>
        </Form>
      </Card>

      <Card
        title="命令行工具（模式 C）"
        style={{ marginTop: 16 }}
        extra={
          <Space>
            <Button type="primary" loading={busy === "cli"} disabled={!!cliError || cli?.installed} onClick={() => cliAction(api.cliInstall, "已安装 pg / pg-claude / pg-codex")}>
              安装到 /usr/local/bin
            </Button>
            <Button loading={busy === "cli"} disabled={!!cliError || !cli?.installed} onClick={() => cliAction(api.cliUninstall, "已卸载")}>
              卸载
            </Button>
          </Space>
        }
      >
        {cliError ? (
          <Alert type="error" showIcon message={cliError} />
        ) : (
          <>
            <Space wrap style={{ marginBottom: 8 }}>
              {cli?.links.map(([n, ok]) => (
                <Tag key={n} color={ok ? "green" : "default"} className="mono">{n} {ok ? "✓" : "✗"}</Tag>
              ))}
            </Space>
            <Typography.Paragraph type="secondary" style={{ fontSize: 12, marginBottom: 0 }}>
              安装后可在任意终端使用 <code>pg claude …</code>、<code>pg codex …</code>、<code>pg run -- &lt;命令&gt;</code>。桌面端运行时 pg 复用其代理与日志；否则 pg 内嵌启动临时代理。
              VS Code 的 Claude Code 扩展可将 <code>claudeCode.claudeProcessWrapper</code> 设为 <code>/usr/local/bin/pg-claude</code>。
            </Typography.Paragraph>
          </>
        )}
      </Card>

      <Card title="路径" style={{ marginTop: 16 }} size="small">
        <Descriptions size="small" column={1}>
          {paths &&
            (Object.entries(paths) as [string, string][]).map(([k, v]) => (
              <Descriptions.Item key={k} label={LABELS[k] ?? k}>
                <a className="mono selectable" style={{ fontSize: 12 }} onClick={() => api.revealPath(v).catch(() => message.info("路径不存在"))}>
                  {v}
                </a>
              </Descriptions.Item>
            ))}
        </Descriptions>
      </Card>
    </div>
  );
}

const LABELS: Record<string, string> = {
  data_dir: "数据目录",
  db_path: "数据库",
  ca_dir: "CA 目录",
  backups_dir: "配置备份",
  claude_settings: "Claude 配置",
  codex_config: "Codex 配置",
};
