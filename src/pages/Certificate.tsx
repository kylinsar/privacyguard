import { useEffect, useState } from "react";
import { Alert, Button, Card, Descriptions, Space, Tag, Typography, App as AntApp, Steps } from "antd";
import { save } from "@tauri-apps/plugin-dialog";
import { api, type CaView, type KeychainKind } from "../api";

export default function Certificate() {
  const [ca, setCa] = useState<CaView | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const { message, modal } = AntApp.useApp();

  const load = () => api.caView().then(setCa).catch((e) => message.error(String(e)));
  useEffect(() => {
    load();
  }, []);

  const run = async (key: string, f: () => Promise<unknown>, ok: string) => {
    setBusy(key);
    try {
      await f();
      message.success(ok);
      await load();
    } catch (e) {
      message.error(String(e), 6);
    } finally {
      setBusy(null);
    }
  };

  const install = (kind: KeychainKind) =>
    run(
      "install-" + kind,
      () => api.caInstall(kind),
      kind === "login" ? "已安装到登录钥匙串并设为信任" : "已安装到系统钥匙串并设为信任",
    );

  const remove = (kind: KeychainKind) => run("remove-" + kind, () => api.caRemove(kind), "已从钥匙串移除");

  const regenerate = () =>
    modal.confirm({
      title: "重新生成根证书？",
      content: "旧证书将失效，已安装到钥匙串的旧证书会尝试移除；之后需要重新安装新证书。运行中的代理会自动重启。",
      okButtonProps: { danger: true },
      onOk: () => run("regen", () => api.caRegenerate(), "已重新生成 CA"),
    });

  const exportPem = async () => {
    const dest = await save({ defaultPath: "pg-root-ca.pem", filters: [{ name: "PEM", extensions: ["pem", "crt"] }] });
    if (dest) await run("export", () => api.caExport(dest), "已导出");
  };

  const trusted = ca?.trust.trusted_for_ssl;
  const step = !ca ? 0 : trusted ? 2 : ca.trust.in_login_keychain || ca.trust.in_system_keychain ? 1 : 0;

  return (
    <div>
      <h1 className="pg-page-title">CA 证书</h1>
      <Alert
        type={trusted ? "success" : "warning"}
        showIcon
        style={{ marginBottom: 16 }}
        message={trusted ? "系统已信任 PrivacyGuard 根证书" : "系统尚未信任 PrivacyGuard 根证书"}
        description={
          trusted
            ? "Claude Code / Codex 以及浏览器等读取系统信任库的程序都能接受代理签发的证书。"
            : "模式 B（系统代理）必须先信任证书；模式 A / C 会通过 NODE_EXTRA_CA_CERTS / CODEX_CA_CERTIFICATE 直接把证书交给工具进程，不装也能用。"
        }
      />
      <Steps
        size="small"
        current={step}
        style={{ marginBottom: 16 }}
        items={[{ title: "已生成" }, { title: "已安装到钥匙串" }, { title: "已被信任" }]}
      />
      <Card
        title="证书信息"
        extra={
          <Space>
            <Button onClick={exportPem} loading={busy === "export"}>导出 PEM</Button>
            <Button onClick={() => ca && api.revealPath(ca.info.cert_path)}>在 Finder 中显示</Button>
            <Button danger onClick={regenerate} loading={busy === "regen"}>重新生成</Button>
          </Space>
        }
      >
        <Descriptions column={1} size="small">
          <Descriptions.Item label="名称">{ca?.info.common_name}</Descriptions.Item>
          <Descriptions.Item label="SHA-256 指纹">
            <span className="mono selectable" style={{ fontSize: 12 }}>{ca?.info.fingerprint_sha256}</span>
          </Descriptions.Item>
          <Descriptions.Item label="SHA-1">
            <span className="mono selectable" style={{ fontSize: 12 }}>{ca?.trust.sha1}</span>
          </Descriptions.Item>
          <Descriptions.Item label="有效期">
            {ca?.info.not_before?.slice(0, 10)} → {ca?.info.not_after?.slice(0, 10)}
          </Descriptions.Item>
          <Descriptions.Item label="文件">
            <span className="mono selectable" style={{ fontSize: 12 }}>{ca?.info.cert_path}</span>
          </Descriptions.Item>
          <Descriptions.Item label="钥匙串">
            <Space>
              <Tag color={ca?.trust.in_login_keychain ? "green" : "default"}>登录钥匙串 {ca?.trust.in_login_keychain ? "✓" : "✗"}</Tag>
              <Tag color={ca?.trust.in_system_keychain ? "green" : "default"}>系统钥匙串 {ca?.trust.in_system_keychain ? "✓" : "✗"}</Tag>
              <Tag color={trusted ? "green" : "red"}>SSL 信任 {trusted ? "✓" : "✗"}</Tag>
            </Space>
          </Descriptions.Item>
        </Descriptions>
      </Card>

      <Card title="安装到钥匙串" style={{ marginTop: 16 }}>
        <Space direction="vertical" style={{ width: "100%" }}>
          <div>
            <Space>
              <Button type="primary" loading={busy === "install-login"} onClick={() => install("login")} disabled={!!ca?.trust.in_login_keychain}>
                安装到登录钥匙串
              </Button>
              <Button loading={busy === "remove-login"} onClick={() => remove("login")} disabled={!ca?.trust.in_login_keychain}>
                移除
              </Button>
              <Typography.Text type="secondary">仅当前用户；会弹出输入账户密码的系统对话框。推荐。</Typography.Text>
            </Space>
          </div>
          <div>
            <Space>
              <Button loading={busy === "install-system"} onClick={() => install("system")} disabled={!!ca?.trust.in_system_keychain}>
                安装到系统钥匙串
              </Button>
              <Button loading={busy === "remove-system"} onClick={() => remove("system")} disabled={!ca?.trust.in_system_keychain}>
                移除
              </Button>
              <Typography.Text type="secondary">所有用户；需要管理员授权。</Typography.Text>
            </Space>
          </div>
          <Alert
            type="info"
            showIcon
            message="手动安装"
            description={
              <span>
                双击 PEM 文件导入「钥匙串访问」，找到「{ca?.info.common_name}」，双击 → 信任 → 「使用此证书时」选择「始终信任」。
              </span>
            }
          />
        </Space>
      </Card>

      <Card title="PEM" style={{ marginTop: 16 }} size="small">
        <pre className="pem selectable">{ca?.pem}</pre>
      </Card>
    </div>
  );
}
