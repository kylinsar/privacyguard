//! 请求处理管线：分类 -> 脱敏 -> 上游 -> 还原 -> 日志。MITM 与反向入口共用。

use crate::body::{empty, full, stream_body, Body, LogFinalizer, RestoringStream};
use crate::ca::CertificateAuthority;
use crate::config::ProxyConfig;
use crate::log::{guess_agent, EntryKind, LogSink, RequestLog};
use crate::upstream::Upstream;
use anyhow::{anyhow, Context as _, Result};
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use parking_lot::RwLock;
use providers::{ApiKind, Provider, SseUsageCollector};
use redact::{RedactionEvent, Redactor};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tokio_rustls::TlsAcceptor;

pub const MAX_DEBUG_BODY: usize = 256 * 1024;

/// 请求来自哪个入口，以及如何确定上游。
#[derive(Debug, Clone)]
pub enum Target {
    /// TLS 终结后的请求；host/port 来自 CONNECT
    Mitm { host: String, port: u16 },
    /// 明文入口：按 reverse_routes 前缀路由，或处理本地端点
    Reverse,
    /// 明文入口的绝对 URI（`GET http://…`），原样转发
    Absolute,
}

/// Upgrade（WebSocket）中继所需的上游信息。
struct UpgradeCtx {
    host: String,
    port: u16,
    tls: bool,
    /// 发往上游的 path?query
    path_q: String,
    path: String,
    entry: EntryKind,
    provider: Provider,
    api: ApiKind,
    store_bodies: bool,
    restore: bool,
}

pub struct Proxy {
    cfg: RwLock<ProxyConfig>,
    ca: Arc<CertificateAuthority>,
    redactor: Arc<Redactor>,
    upstream: Upstream,
    sink: Arc<dyn LogSink>,
    acceptor: TlsAcceptor,
    metrics: Arc<Metrics>,
}

impl Proxy {
    pub fn new(
        cfg: ProxyConfig,
        ca: Arc<CertificateAuthority>,
        redactor: Arc<Redactor>,
        sink: Arc<dyn LogSink>,
    ) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let upstream = Upstream::new(&cfg)?;
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(ca.clone());
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            cfg: RwLock::new(cfg),
            ca,
            redactor,
            upstream,
            sink,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            metrics: Arc::new(Metrics::default()),
        })
    }

    /// 实时计数器（在途请求数等）。
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    pub fn config(&self) -> ProxyConfig {
        self.cfg.read().clone()
    }

    /// 热更新（listen / upstream_proxy 变更需重启代理才生效）。
    pub fn update_config(&self, cfg: ProxyConfig) {
        *self.cfg.write() = cfg;
    }

    pub fn ca(&self) -> &Arc<CertificateAuthority> {
        &self.ca
    }

    pub fn redactor(&self) -> &Arc<Redactor> {
        &self.redactor
    }

    pub fn acceptor(&self) -> &TlsAcceptor {
        &self.acceptor
    }

    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    pub fn sink(&self) -> &Arc<dyn LogSink> {
        &self.sink
    }

    /// 处理一个已解码的 HTTP 请求。永不 panic；错误转为 4xx/5xx 响应。
    pub async fn handle_http(self: &Arc<Self>, req: Request<Incoming>, target: Target) -> Response<Body> {
        let started = Instant::now();
        let _guard = InflightGuard::new(self.metrics.clone());
        match self.handle_inner(req, target, started).await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("请求处理失败: {e:#}");
                error_response(StatusCode::BAD_GATEWAY, &format!("PrivacyGuard: {e:#}"))
            }
        }
    }

    async fn handle_inner(
        self: &Arc<Self>,
        req: Request<Incoming>,
        target: Target,
        started: Instant,
    ) -> Result<Response<Body>> {
        let cfg = self.config();
        let path_q = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        let path = path_q.split('?').next().unwrap_or("/").to_string();

        // 解析上游 URL 与入口类型
        // upstream_path: 发往上游的 path（反向入口会去掉前缀），用于分类与 Upgrade 中继
        let (url, host, (port, upstream_tls), upstream_path_q, entry) = match &target {
            Target::Mitm { host, port } => {
                let authority = if *port == 443 {
                    host.clone()
                } else {
                    format!("{host}:{port}")
                };
                (
                    format!("https://{authority}{path_q}"),
                    host.clone(),
                    (*port, true),
                    path_q.clone(),
                    EntryKind::Mitm,
                )
            }
            Target::Reverse => {
                if let Some(resp) = self.local_endpoint(&path, &cfg) {
                    return Ok(resp);
                }
                let Some((route, rest)) = cfg.reverse_route(&path) else {
                    return Ok(error_response(
                        StatusCode::NOT_FOUND,
                        "PrivacyGuard: 未匹配到反向路由。可用前缀: /openai /anthropic /chatgpt",
                    ));
                };
                let query = path_q.split_once('?').map(|(_, q)| format!("?{q}")).unwrap_or_default();
                let up: Uri = route.upstream.parse().context("反向路由 upstream 无效")?;
                let host = up.host().unwrap_or_default().to_string();
                let tls = up.scheme_str() != Some("http");
                let port = up.port_u16().unwrap_or(if tls { 443 } else { 80 });
                let base_path = up.path().trim_end_matches('/');
                (
                    format!("{}{rest}{query}", route.upstream.trim_end_matches('/')),
                    host,
                    (port, tls),
                    format!("{base_path}{rest}{query}"),
                    EntryKind::Reverse,
                )
            }
            Target::Absolute => {
                let host = req.uri().host().unwrap_or_default().to_string();
                let port = req.uri().port_u16().unwrap_or(80);
                (req.uri().to_string(), host, (port, false), path_q.clone(), EntryKind::Reverse)
            }
        };
        let upstream_path = upstream_path_q.split('?').next().unwrap_or("/").to_string();

        let (provider, api) = match &target {
            Target::Absolute => (Provider::Unknown, ApiKind::Unknown),
            _ => providers::classify(&host, &upstream_path),
        };

        // WebSocket 等 Upgrade 请求：与上游握手后中继；对话类接口做消息级脱敏
        if is_upgrade(req.headers()) {
            if matches!(target, Target::Absolute) {
                return Ok(error_response(StatusCode::NOT_IMPLEMENTED, "PrivacyGuard: 不支持明文绝对 URI 的 Upgrade"));
            }
            let ctx = UpgradeCtx {
                host,
                port,
                tls: upstream_tls,
                path_q: upstream_path_q,
                path: upstream_path,
                entry,
                provider,
                api,
                store_bodies: cfg.store_redacted_bodies,
                restore: cfg.restore_responses,
            };
            return self.relay_upgrade(req, ctx, started).await;
        }
        let method = req.method().clone();
        let ua = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let mut log = RequestLog {
            id: uuid::Uuid::new_v4().to_string(),
            started_at: chrono::Utc::now(),
            entry,
            agent: guess_agent(ua.as_deref(), provider),
            provider,
            api,
            method: method.to_string(),
            host: host.clone(),
            path: path.clone(),
            session_id: session_from_headers(req.headers()),
            ..Default::default()
        };

        // 读取请求体
        let (parts, body) = req.into_parts();
        let body_bytes = body
            .collect()
            .await
            .map_err(|e| anyhow!("读取请求体失败: {e}"))?
            .to_bytes();
        log.req_bytes = body_bytes.len() as u64;

        // 脱敏
        let transform = api.is_chat() && method == Method::POST && is_json(&parts.headers);
        let mut decoded_request = false;
        let (out_body, events) = if transform {
            // Codex 会用 zstd 压缩请求体；先按 content-encoding 解开
            let enc = crate::encoding::parse(
                parts
                    .headers
                    .get(header::CONTENT_ENCODING)
                    .and_then(|v| v.to_str().ok()),
            );
            let plain = match crate::encoding::decode(enc, &body_bytes) {
                Ok(p) => {
                    decoded_request = enc != crate::encoding::BodyEncoding::Identity;
                    Some(Bytes::from(p))
                }
                Err(e) => {
                    log.warnings.push(format!("请求体 content-encoding 无法解码，已原样转发: {e}"));
                    None
                }
            };
            match plain.as_deref().map(|p| self.redact_json(p, api)) {
                Some(Ok((b, events, meta))) => {
                    if log.session_id.is_none() {
                        log.session_id = meta.session_id.clone();
                    }
                    log.request_meta = Some(meta);
                    log.streamed = log.request_meta.as_ref().is_some_and(|m| m.stream);
                    if cfg.store_redacted_bodies {
                        let s = String::from_utf8_lossy(&b);
                        log.redacted_body = Some(s.chars().take(MAX_DEBUG_BODY).collect());
                    }
                    (b, events)
                }
                Some(Err(e)) => {
                    log.warnings.push(format!("请求体不是合法 JSON，已原样转发: {e}"));
                    decoded_request = false;
                    (body_bytes, Vec::new())
                }
                None => (body_bytes, Vec::new()),
            }
        } else {
            (body_bytes, Vec::new())
        };
        log.redaction_events = events;

        // 组装上游请求
        let mut headers = filter_request_headers(&parts.headers);
        if transform {
            // 我们要改写响应，禁止压缩
            headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        }
        if decoded_request {
            // 已解压并改写，以明文转发
            headers.remove(header::CONTENT_ENCODING);
        }
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(out_body.len()));
        let up_req = self
            .upstream
            .client()
            .request(method.clone(), &url)
            .headers(headers)
            .body(out_body)
            .build()
            .context("构建上游请求")?;

        let mut fin = LogFinalizer::new(log, started, self.sink.clone());
        let resp = match self.upstream.client().execute(up_req).await {
            Ok(r) => r,
            Err(e) => {
                fin.log_mut().error = Some(format!("上游请求失败: {e}"));
                return Ok(error_response(StatusCode::BAD_GATEWAY, &format!("PrivacyGuard 上游请求失败: {e}")));
            }
        };

        let status = resp.status();
        fin.mark_first_byte();
        fin.log_mut().status = Some(status.as_u16());
        let mut resp_headers = filter_response_headers(resp.headers());
        let is_sse = content_type(resp.headers()).starts_with("text/event-stream");

        if !transform {
            // 透传：直接流式转发
            resp_headers.remove(header::CONTENT_LENGTH);
            if let Some(len) = resp.content_length() {
                resp_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            }
            let stream = RestoringStream::new(Box::pin(resp.bytes_stream()), None, None, fin);
            return Ok(build_response(status, resp_headers, stream_body(stream)));
        }

        if !cfg.restore_responses {
            fin.log_mut().warnings.push("已按设置保留模型输出中的占位符，未还原".into());
        }

        if is_sse {
            resp_headers.remove(header::CONTENT_LENGTH);
            let restorer: Option<Box<dyn crate::body::ChunkRestorer>> = if cfg.restore_responses {
                Some(Box::new(crate::sse_restore::SseRestorer::new(api, self.redactor.map())))
            } else {
                None
            };
            let stream = RestoringStream::new(
                Box::pin(resp.bytes_stream()),
                restorer,
                Some(SseUsageCollector::new(api)),
                fin,
            );
            return Ok(build_response(status, resp_headers, stream_body(stream)));
        }

        // 非流式：整体缓冲并还原
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                fin.log_mut().error = Some(format!("读取上游响应失败: {e}"));
                return Ok(error_response(StatusCode::BAD_GATEWAY, "PrivacyGuard: 读取上游响应失败"));
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let (restored, n) = if cfg.restore_responses {
            self.redactor.restore_text(&text, true)
        } else {
            (text.to_string(), 0)
        };
        {
            let log = fin.log_mut();
            log.restored_count = n;
            log.resp_bytes = restored.len() as u64;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&restored) {
                let u = providers::parse_usage_json(api, &v);
                if !u.is_empty() {
                    if let Some(err) = &u.error {
                        log.error = Some(err.clone());
                    }
                    log.usage = Some(u);
                }
            }
        }
        resp_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(restored.len()));
        fin.finish();
        Ok(build_response(status, resp_headers, full(restored)))
    }

    fn redact_json(
        &self,
        body: &[u8],
        api: ApiKind,
    ) -> Result<(Bytes, Vec<RedactionEvent>, providers::RequestMeta)> {
        let mut value: serde_json::Value = serde_json::from_slice(body)?;
        let meta = providers::request_meta(api, &value);
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        let redactor = self.redactor.clone();
        let mut f = |s: &str| -> String {
            let out = redactor.redact_text(s);
            for e in out.events {
                *counts.entry((e.rule_id, e.entity_type)).or_insert(0) += e.count;
            }
            out.text
        };
        providers::redact_request_body(api, &mut value, &mut f);
        let events = counts
            .into_iter()
            .map(|((rule_id, entity_type), count)| RedactionEvent {
                rule_id,
                entity_type,
                count,
            })
            .collect();
        Ok((Bytes::from(serde_json::to_vec(&value)?), events, meta))
    }

    /// 本地端点：PAC、CA 下载、健康检查。
    fn local_endpoint(&self, path: &str, cfg: &ProxyConfig) -> Option<Response<Body>> {
        match path {
            "/proxy.pac" | "/pg/proxy.pac" => {
                let hosts = cfg.pac_hosts.as_ref().unwrap_or(&cfg.intercept_hosts);
                let pac = crate::pac::generate(hosts, cfg.listen);
                Some(build_response(
                    StatusCode::OK,
                    hdrs(&[("content-type", "application/x-ns-proxy-autoconfig")]),
                    full(pac),
                ))
            }
            "/pg/ca.pem" => Some(build_response(
                StatusCode::OK,
                hdrs(&[
                    ("content-type", "application/x-pem-file"),
                    ("content-disposition", "attachment; filename=\"pg-root-ca.pem\""),
                ]),
                full(self.ca.cert_pem().to_string()),
            )),
            "/pg/health" | "/" => {
                let body = serde_json::json!({
                    "name": "PrivacyGuard",
                    "version": env!("CARGO_PKG_VERSION"),
                    "listen": cfg.listen.to_string(),
                    "intercept_hosts": cfg.intercept_hosts,
                    "ca_fingerprint_sha256": self.ca.fingerprint_sha256(),
                });
                Some(build_response(
                    StatusCode::OK,
                    hdrs(&[("content-type", "application/json")]),
                    full(body.to_string()),
                ))
            }
            _ => None,
        }
    }

    /// WebSocket 等 Upgrade 请求的中继：与上游握手，成功后
    /// - 对话类接口（如 Responses WebSocket 模式）：消息级脱敏 / 还原；
    /// - 其他：双向原样拷贝。
    async fn relay_upgrade(
        self: &Arc<Self>,
        mut req: Request<Incoming>,
        ctx: UpgradeCtx,
        started: Instant,
    ) -> Result<Response<Body>> {
        let is_ws = req
            .headers()
            .get(header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
        let transform = is_ws && ctx.api.is_chat();

        let mut log = RequestLog {
            id: uuid::Uuid::new_v4().to_string(),
            started_at: chrono::Utc::now(),
            entry: ctx.entry,
            agent: guess_agent(
                req.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok()),
                ctx.provider,
            ),
            provider: ctx.provider,
            api: ctx.api,
            method: req.method().to_string(),
            host: ctx.host.clone(),
            path: ctx.path.clone(),
            ..Default::default()
        };
        if !transform {
            log.warnings.push("WebSocket/Upgrade 连接已透传，未脱敏".into());
        } else if !ctx.restore {
            log.warnings.push("已按设置保留模型输出中的占位符，未还原".into());
        }
        let template = log.clone();
        let mut fin = LogFinalizer::new(log, started, self.sink.clone());

        let io = self.upstream.connect(ctx.tls, &ctx.host, ctx.port).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .context("上游 HTTP/1.1 握手")?;
        tokio::spawn(async move {
            if let Err(e) = conn.with_upgrades().await {
                tracing::debug!("上游 upgrade 连接结束: {e}");
            }
        });

        let downstream_upgrade = hyper::upgrade::on(&mut req);
        let (parts, body) = req.into_parts();
        let body_bytes = body.collect().await.map_err(|e| anyhow!("{e}"))?.to_bytes();
        let mut up = Request::builder().method(parts.method).uri(ctx.path_q.as_str());
        for (k, v) in &parts.headers {
            if k == header::HOST {
                continue;
            }
            // 要改写消息内容时禁用 permessage-deflate 等扩展，保证帧是明文 JSON
            if transform && k == header::SEC_WEBSOCKET_EXTENSIONS {
                continue;
            }
            up = up.header(k, v);
        }
        up = up.header(header::HOST, ctx.host.as_str());
        let up_req = up.body(full(body_bytes))?;
        let mut resp = sender.send_request(up_req).await.context("上游 Upgrade 请求失败")?;
        let status = resp.status();
        fin.log_mut().status = Some(status.as_u16());

        if status == StatusCode::SWITCHING_PROTOCOLS {
            let upstream_upgrade = hyper::upgrade::on(&mut resp);
            let mut headers = resp.headers().clone();
            if transform {
                headers.remove(header::SEC_WEBSOCKET_EXTENSIONS);
            }
            let proxy = self.clone();
            tokio::spawn(async move {
                let (down, up) = match (downstream_upgrade.await, upstream_upgrade.await) {
                    (Ok(d), Ok(u)) => (d, u),
                    (d, u) => {
                        tracing::warn!("upgrade 失败: down={:?} up={:?}", d.err(), u.err());
                        return;
                    }
                };
                let mut down = TokioIo::new(down);
                let mut up = TokioIo::new(up);
                if transform {
                    let session = crate::ws::WsSession::new(
                        ctx.api,
                        proxy.redactor.clone(),
                        proxy.sink.clone(),
                        template,
                        ctx.store_bodies,
                        ctx.restore,
                    );
                    let (a, b) = crate::ws::relay(down, up, session).await;
                    fin.log_mut().req_bytes = a;
                    fin.log_mut().resp_bytes = b;
                } else {
                    match tokio::io::copy_bidirectional(&mut down, &mut up).await {
                        Ok((a, b)) => {
                            fin.log_mut().req_bytes = a;
                            fin.log_mut().resp_bytes = b;
                        }
                        Err(e) => fin.log_mut().error = Some(format!("upgrade 中继中断: {e}")),
                    }
                }
                fin.finish();
            });
            return Ok(build_response(status, headers, empty()));
        }

        // 非 101：普通响应，缓冲后返回
        let headers = filter_response_headers(resp.headers());
        let bytes = resp.into_body().collect().await.map_err(|e| anyhow!("{e}"))?.to_bytes();
        fin.log_mut().resp_bytes = bytes.len() as u64;
        fin.finish();
        Ok(build_response(status, headers, full(bytes)))
    }
}

fn is_upgrade(h: &HeaderMap) -> bool {
    h.get(header::UPGRADE).is_some()
        || h
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"))
}

fn is_json(h: &HeaderMap) -> bool {
    content_type(h).starts_with("application/json") || content_type(h).is_empty()
}

fn content_type(h: &HeaderMap) -> String {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase()
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn filter_request_headers(h: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in h {
        let name = k.as_str();
        if HOP_BY_HOP.contains(&name) || name == "host" || name == "content-length" {
            continue;
        }
        out.append(k.clone(), v.clone());
    }
    out
}

fn filter_response_headers(h: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in h {
        let name = k.as_str();
        if HOP_BY_HOP.contains(&name) || name == "content-length" {
            continue;
        }
        out.append(k.clone(), v.clone());
    }
    out
}

fn hdrs(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        if let Ok(val) = HeaderValue::from_str(v) {
            h.insert(HeaderName::from_static(k), val);
        }
    }
    h
}

fn build_response(status: StatusCode, headers: HeaderMap, body: Body) -> Response<Body> {
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

pub fn error_response(status: StatusCode, msg: &str) -> Response<Body> {
    build_response(
        status,
        hdrs(&[("content-type", "text/plain; charset=utf-8")]),
        full(msg.to_string()),
    )
}


/// 进程内实时计数器。
#[derive(Debug, Default)]
pub struct Metrics {
    pub inflight: std::sync::atomic::AtomicUsize,
    pub total_requests: std::sync::atomic::AtomicU64,
}

struct InflightGuard(Arc<Metrics>);

impl InflightGuard {
    fn new(m: Arc<Metrics>) -> Self {
        use std::sync::atomic::Ordering;
        m.inflight.fetch_add(1, Ordering::Relaxed);
        m.total_requests.fetch_add(1, Ordering::Relaxed);
        Self(m)
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 从请求头提取会话 id（Codex 发送 `session_id`，部分客户端用 `x-session-id`）。
fn session_from_headers(h: &http::HeaderMap) -> Option<String> {
    ["session_id", "x-session-id", "x-conversation-id"]
        .iter()
        .find_map(|k| h.get(*k).and_then(|v| v.to_str().ok()))
        .map(|s| s.chars().take(64).collect())
}
