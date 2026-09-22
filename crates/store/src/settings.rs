use serde::{Deserialize, Serialize};

/// 应用级设置（持久化在 settings 表，key = "app"）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppSettings {
    pub port: u16,
    pub intercept_hosts: Vec<String>,
    pub upstream_proxy: Option<String>,
    pub store_redacted_bodies: bool,
    /// 是否把模型输出中的占位符还原为原值（关闭后用户直接看到 PG_xxx 占位符）
    pub restore_responses: bool,
    pub log_retention_days: u32,
    pub autostart: bool,
    pub start_proxy_on_launch: bool,
    pub minimize_to_tray: bool,
    /// 当前启用的隐身身份 id（None 表示不启用）
    pub active_persona: Option<String>,
    /// 未被身份覆盖的敏感值的替换风格
    pub substitution_style: redact::Style,
    /// 模型定价表（空表示使用内置参考价）
    pub pricing: Vec<crate::pricing::ModelPrice>,
}

impl AppSettings {
    /// 生效的定价表：用户未自定义时用内置参考价。
    pub fn effective_prices(&self) -> Vec<crate::pricing::ModelPrice> {
        if self.pricing.is_empty() {
            crate::pricing::default_prices()
        } else {
            self.pricing.clone()
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            port: proxy_core::DEFAULT_PORT,
            intercept_hosts: providers::classify::INTERCEPT_HOSTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            upstream_proxy: None,
            store_redacted_bodies: false,
            restore_responses: true,
            log_retention_days: 30,
            autostart: false,
            start_proxy_on_launch: true,
            minimize_to_tray: true,
            active_persona: None,
            substitution_style: redact::Style::Placeholder,
            pricing: Vec::new(),
        }
    }
}

impl AppSettings {
    pub fn to_proxy_config(&self) -> proxy_core::ProxyConfig {
        let mut cfg = proxy_core::ProxyConfig::default();
        cfg.listen.set_port(self.port);
        cfg.intercept_hosts = self.intercept_hosts.clone();
        cfg.upstream_proxy = self.upstream_proxy.clone().filter(|s| !s.trim().is_empty());
        cfg.store_redacted_bodies = self.store_redacted_bodies;
        cfg.restore_responses = self.restore_responses;
        cfg
    }
}

/// 一套隐身身份：若干「真实值 -> 隐身值」字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub description: String,
    pub fields: Vec<redact::IdentityField>,
    pub updated_at: String,
}

/// 某个工具的保护模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionMode {
    /// A：写入工具自身配置
    Config,
    /// B：系统 PAC + 系统证书
    SystemProxy,
    /// C：包装启动器
    Launcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    pub fn key(&self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
    pub fn all() -> [Agent; 2] {
        [Agent::Claude, Agent::Codex]
    }
}

/// 某个工具当前的保护状态（activation_state 表，key = agent）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct AgentActivation {
    pub enabled: bool,
    pub mode: Option<ProtectionMode>,
    /// 平台层写入的回滚信息（备份路径、原始值等），结构由平台层定义
    pub rollback: serde_json::Value,
    pub activated_at: Option<String>,
}
