//! PrivacyGuard 代理核心：本地 MITM / 反向代理，脱敏后转发到 LLM 提供商并还原响应。

pub mod body;
pub mod ca;
pub mod config;
pub mod encoding;
pub mod log;
pub mod pac;
pub mod pipeline;
pub mod server;
pub mod sse_restore;
pub mod upstream;
pub mod ws;

pub use ca::{CaInfo, CertificateAuthority};
pub use config::{ProxyConfig, ReverseRoute, DEFAULT_PORT};
pub use log::{EntryKind, LogSink, NoopSink, RequestLog, TracingSink};
pub use pipeline::{Metrics, Proxy, Target};
pub use server::{spawn, ProxyHandle};

use anyhow::Result;
use redact::Redactor;
use std::sync::Arc;

/// 一步构建并启动代理。
pub async fn start(
    cfg: ProxyConfig,
    ca: Arc<CertificateAuthority>,
    redactor: Arc<Redactor>,
    sink: Arc<dyn LogSink>,
) -> Result<(Arc<Proxy>, ProxyHandle)> {
    let proxy = Arc::new(Proxy::new(cfg, ca, redactor, sink)?);
    let handle = spawn(proxy.clone()).await?;
    Ok((proxy, handle))
}
