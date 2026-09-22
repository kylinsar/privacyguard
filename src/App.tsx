import React, { useEffect, useState } from "react";
import { Layout, Menu, App as AntApp } from "antd";
import {
  DashboardOutlined,
  SafetyCertificateOutlined,
  SafetyOutlined,
  FilterOutlined,
  UnorderedListOutlined,
  SettingOutlined,
  UserSwitchOutlined,
  LineChartOutlined,
} from "@ant-design/icons";
import { api, onAppNotes, onProxyStatus, type ProxyStatus } from "./api";
import Overview from "./pages/Overview";
import Protection from "./pages/Protection";
import Certificate from "./pages/Certificate";
import Rules from "./pages/Rules";
import Personas from "./pages/Personas";
import Monitor from "./pages/Monitor";
import Logs from "./pages/Logs";
import Settings from "./pages/Settings";

type PageKey = "overview" | "protection" | "certificate" | "rules" | "personas" | "monitor" | "logs" | "settings";

export default function App() {
  const [page, setPage] = useState<PageKey>("overview");
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const { notification } = AntApp.useApp();

  const refreshStatus = () => api.proxyStatus().then(setStatus).catch(() => {});

  useEffect(() => {
    refreshStatus();
    const timer = setInterval(refreshStatus, 2000);
    const un1 = onProxyStatus(setStatus);
    const un2 = onAppNotes((notes) => {
      notes.forEach((n) => notification.info({ message: "启动自检", description: n }));
    });
    return () => {
      clearInterval(timer);
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, []);

  const pages: Record<PageKey, React.ReactNode> = {
    overview: <Overview status={status} onStatus={setStatus} goto={(p) => setPage(p as PageKey)} />,
    protection: <Protection status={status} onStatus={setStatus} />,
    certificate: <Certificate />,
    rules: <Rules />,
    personas: <Personas />,
    monitor: <Monitor status={status} />,
    logs: <Logs />,
    settings: <Settings onStatus={setStatus} />,
  };

  return (
    <Layout className="pg-layout">
      <Layout.Sider width={200} className="pg-sider" theme="dark">
        <div className="brand">
          <span className={"dot" + (status?.running ? " on" : "")} />
          PrivacyGuard
        </div>
        <Menu
          theme="dark"
          mode="inline"
          selectedKeys={[page]}
          onClick={(e) => setPage(e.key as PageKey)}
          items={[
            { key: "overview", icon: <DashboardOutlined />, label: "概览" },
            { key: "protection", icon: <SafetyOutlined />, label: "保护" },
            { key: "certificate", icon: <SafetyCertificateOutlined />, label: "证书" },
            { key: "rules", icon: <FilterOutlined />, label: "规则" },
            { key: "personas", icon: <UserSwitchOutlined />, label: "隐身身份" },
            { key: "monitor", icon: <LineChartOutlined />, label: "使用监控" },
            { key: "logs", icon: <UnorderedListOutlined />, label: "日志" },
            { key: "settings", icon: <SettingOutlined />, label: "设置" },
          ]}
        />
      </Layout.Sider>
      <Layout.Content className="pg-content">{pages[page]}</Layout.Content>
    </Layout>
  );
}
