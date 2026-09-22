//! 上游连接：reqwest 客户端（HTTP 请求）与裸 TLS 连接（WebSocket 中继）。

use crate::config::ProxyConfig;
use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub struct Upstream {
    client: reqwest::Client,
    tls: TlsConnector,
}

impl Upstream {
    pub fn new(cfg: &ProxyConfig) -> Result<Self> {
        let mut b = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent("PrivacyGuard/0.1");
        b = match &cfg.upstream_proxy {
            Some(p) => b.proxy(reqwest::Proxy::all(p).context("上游代理地址无效")?),
            // 显式禁用环境变量代理，避免把自己当上游形成回环
            None => b.no_proxy(),
        };
        let client = b.build().context("构建上游 HTTP 客户端")?;

        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for e in &native.errors {
            tracing::warn!("加载系统根证书出错: {e}");
        }
        for c in native.certs {
            let _ = roots.add(c);
        }
        let mut tls_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            client,
            tls: TlsConnector::from(Arc::new(tls_cfg)),
        })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn connect_tls(&self, host: &str, port: u16) -> Result<TlsStream<TcpStream>> {
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("连接上游 {host}:{port}"))?;
        let name = ServerName::try_from(host.to_string()).context("上游 SNI 无效")?;
        let tls = self.tls.connect(name, tcp).await.context("上游 TLS 握手")?;
        Ok(tls)
    }

    /// 按 scheme 连接上游：https 走 TLS，http（仅测试 / 本地网关）走明文。
    pub async fn connect(&self, tls: bool, host: &str, port: u16) -> Result<BoxedIo> {
        if tls {
            Ok(Box::new(self.connect_tls(host, port).await?))
        } else {
            let tcp = TcpStream::connect((host, port))
                .await
                .with_context(|| format!("连接上游 {host}:{port}"))?;
            Ok(Box::new(tcp))
        }
    }
}

pub trait AsyncIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> AsyncIo for T {}
pub type BoxedIo = Box<dyn AsyncIo>;
