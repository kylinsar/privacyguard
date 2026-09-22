use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const DEFAULT_PORT: u16 = 7890;

/// 反向代理入口的路由：`/<prefix>/rest` -> `<upstream>/rest`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReverseRoute {
    pub prefix: String,
    /// 形如 `https://api.openai.com`，不带尾部 `/`
    pub upstream: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub listen: SocketAddr,
    /// 需要 TLS 终结并脱敏的域名；其他 CONNECT 走原样隧道
    pub intercept_hosts: Vec<String>,
    pub reverse_routes: Vec<ReverseRoute>,
    /// 上游代理（如企业代理 / 本地翻墙工具）。None 表示直连并忽略环境变量。
    pub upstream_proxy: Option<String>,
    /// 调试：在日志中保留脱敏后的请求体
    pub store_redacted_bodies: bool,
    /// 占位符映射表容量
    pub placeholder_capacity: usize,
    /// PAC 中走代理的域名；None 时使用 intercept_hosts（模式 B 按已启用的工具收窄）
    #[serde(default)]
    pub pac_hosts: Option<Vec<String>>,
    /// 是否把模型输出中的占位符还原为原值。关闭后用户会直接看到 `PG_EMAIL_xxxx`，
    /// 便于确认模型实际拿到的内容。
    #[serde(default = "default_true")]
    pub restore_responses: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT),
            intercept_hosts: providers::classify::INTERCEPT_HOSTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            reverse_routes: default_reverse_routes(),
            upstream_proxy: None,
            store_redacted_bodies: false,
            placeholder_capacity: 50_000,
            pac_hosts: None,
            restore_responses: true,
        }
    }
}

pub fn default_reverse_routes() -> Vec<ReverseRoute> {
    vec![
        ReverseRoute {
            prefix: "openai".into(),
            upstream: "https://api.openai.com".into(),
        },
        ReverseRoute {
            prefix: "anthropic".into(),
            upstream: "https://api.anthropic.com".into(),
        },
        ReverseRoute {
            prefix: "chatgpt".into(),
            upstream: "https://chatgpt.com".into(),
        },
    ]
}

impl ProxyConfig {
    pub fn should_intercept(&self, host: &str) -> bool {
        let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
        self.intercept_hosts
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&h) || h.ends_with(&format!(".{}", x.to_ascii_lowercase())))
    }

    /// 匹配反向路由，返回 (上游 base, 去掉前缀后的路径)
    pub fn reverse_route<'a>(&self, path: &'a str) -> Option<(&ReverseRoute, &'a str)> {
        let trimmed = path.strip_prefix('/')?;
        for r in &self.reverse_routes {
            if let Some(rest) = trimmed.strip_prefix(r.prefix.as_str()) {
                if rest.is_empty() || rest.starts_with('/') {
                    return Some((r, if rest.is_empty() { "/" } else { rest }));
                }
            }
        }
        None
    }
}
