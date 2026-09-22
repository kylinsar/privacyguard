use providers::{ApiKind, Provider, RequestMeta, UsageMeta};
use redact::RedactionEvent;
use serde::{Deserialize, Serialize};

/// 流量进入代理的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// CONNECT + TLS 终结（模式 A-Claude / B / C）
    #[default]
    Mitm,
    /// 明文反向代理（模式 A-Codex）
    Reverse,
    /// 非拦截域名的原样隧道
    Tunnel,
}

/// 一次完整请求的记录（不含任何原始隐私值）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequestLog {
    pub id: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub entry: EntryKind,
    /// 由启动器 / 配置写入的 User-Agent 或环境推断出的客户端标识（claude / codex / unknown）
    pub agent: String,
    pub provider: Provider,
    pub api: ApiKind,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub req_bytes: u64,
    pub resp_bytes: u64,
    pub streamed: bool,
    pub redaction_events: Vec<RedactionEvent>,
    pub restored_count: usize,
    /// 首字节延迟（上游响应头到达 / WS 首个服务端事件）
    pub ttft_ms: Option<u64>,
    /// 会话标识（来自请求头 session_id 或请求体）
    pub session_id: Option<String>,
    pub request_meta: Option<RequestMeta>,
    pub usage: Option<UsageMeta>,
    pub error: Option<String>,
    /// 仅调试模式下填充：脱敏后的请求体
    pub redacted_body: Option<String>,
    /// 如 WebSocket 透传未脱敏
    pub warnings: Vec<String>,
}

impl RequestLog {
    pub fn redaction_count(&self) -> usize {
        self.redaction_events.iter().map(|e| e.count).sum()
    }
}

/// 日志消费者。实现方（Tauri 应用 / CLI）负责落库与 UI 推送，必须快速返回。
pub trait LogSink: Send + Sync {
    fn record(&self, log: RequestLog);
}

pub struct NoopSink;
impl LogSink for NoopSink {
    fn record(&self, _log: RequestLog) {}
}

/// 把日志打到 tracing。
pub struct TracingSink;
impl LogSink for TracingSink {
    fn record(&self, log: RequestLog) {
        tracing::info!(
            target: "pg::request",
            id = %log.id,
            agent = %log.agent,
            provider = log.provider.as_str(),
            method = %log.method,
            host = %log.host,
            path = %log.path,
            status = ?log.status,
            ms = log.duration_ms,
            redacted = log.redaction_count(),
            restored = log.restored_count,
            model = ?log.request_meta.as_ref().and_then(|m| m.model.clone()),
            error = ?log.error,
            "request"
        );
    }
}

/// 根据 User-Agent 推断客户端。
pub fn guess_agent(user_agent: Option<&str>, provider: Provider) -> String {
    let ua = user_agent.unwrap_or("").to_ascii_lowercase();
    if ua.contains("claude") {
        return "claude".into();
    }
    if ua.contains("codex") {
        return "codex".into();
    }
    match provider {
        Provider::Anthropic => "claude".into(),
        Provider::ChatGptCodex => "codex".into(),
        _ => "unknown".into(),
    }
}
