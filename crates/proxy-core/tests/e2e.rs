//! 端到端：客户端 -> 反向入口 -> 脱敏 -> mock 上游（校验无明文）-> SSE/JSON 响应 -> 还原。

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use proxy_core::{CertificateAuthority, LogSink, ProxyConfig, RequestLog, ReverseRoute};
use redact::{builtin_rules, Redactor};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

const EMAIL: &str = "alice@corp-secret.com";
const KEY: &str = "sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// mock 上游：断言请求体不含明文，并把收到的所有占位符原样回显（模拟模型复述）。
async fn mock_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service_fn(handle_mock))
                    .with_upgrades()
                    .await;
            });
        }
    });
    addr
}

async fn handle_mock(
    mut req: Request<hyper::body::Incoming>,
) -> Result<Response<http_body_util::combinators::BoxBody<Bytes, Infallible>>, Infallible> {
    let path = req.uri().path().to_string();
    if req.headers().contains_key("upgrade") {
        return Ok(handle_mock_ws(&mut req));
    }
    assert!(
        req.headers().get("content-encoding").is_none(),
        "上游不应收到压缩的请求体"
    );
    let accept_encoding = req
        .headers()
        .get("accept-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = req.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(!text.contains(EMAIL), "上游收到了明文邮箱: {text}");
    assert!(!text.contains(KEY), "上游收到了明文密钥: {text}");
    assert_eq!(accept_encoding, "identity");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();

    let placeholders: Vec<String> = redact::placeholder_regex()
        .find_iter(&text)
        .map(|m| m.as_str().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut echo = placeholders.join(" and ");
    // 身份测试：请求带 echo_all 时，把（已脱敏的）用户文本整体回显
    if v["echo_all"].as_bool() == Some(true) {
        let t = v["messages"][0]["content"][0]["text"].as_str().unwrap_or("");
        assert!(!t.contains("王大锤") && !t.contains("真实公司"), "上游收到了真实身份: {t}");
        echo = t.to_string();
    }

    if path.ends_with("/v1/messages") && v["stream"].as_bool() == Some(true) {
        // Anthropic 风格 SSE，故意把占位符拆到多个事件里
        let mut events = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"mock\",\"usage\":{\"input_tokens\":11}}}\n\n".to_string(),
        ];
        let mut mid = echo.len() / 2;
        while !echo.is_char_boundary(mid) {
            mid += 1;
        }
        let (a, b) = echo.split_at(mid);
        for part in [format!("Your email is {a}"), format!("{b} ok")] {
            let j = serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":part}});
            events.push(format!("event: content_block_delta\ndata: {j}\n\n"));
        }
        events.push("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n".to_string());
        let stream = futures_util::stream::iter(
            events
                .into_iter()
                .map(|e| Ok::<_, Infallible>(Frame::data(Bytes::from(e)))),
        );
        let resp = Response::builder()
            .header("content-type", "text/event-stream")
            .body(StreamBody::new(stream).boxed())
            .unwrap();
        return Ok(resp);
    }
    let j = serde_json::json!({
        "id":"msg_1","type":"message","model":"mock","stop_reason":"end_turn",
        "content":[{"type":"text","text":format!("echo: {echo}")}],
        "usage":{"input_tokens":3,"output_tokens":4}
    });
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(j.to_string())).boxed())
        .unwrap())
}

/// mock 的 Responses WebSocket 模式上游：收到 `response.create` 后断言无明文，
/// 把占位符拆成两个 delta 事件回显，再发 `response.completed`。
fn handle_mock_ws(
    req: &mut Request<hyper::body::Incoming>,
) -> Response<http_body_util::combinators::BoxBody<Bytes, Infallible>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;

    assert!(
        req.headers().get("sec-websocket-extensions").is_none(),
        "代理应去掉 WebSocket 扩展协商"
    );
    let key = req.headers().get("sec-websocket-key").unwrap().as_bytes().to_vec();
    let accept = derive_accept_key(&key);
    let upgrade = hyper::upgrade::on(req);
    tokio::spawn(async move {
        let io = TokioIo::new(upgrade.await.unwrap());
        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(io, Role::Server, None).await;
        let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("expected text frame") };
        assert!(!t.contains(EMAIL), "WS 上游收到明文邮箱: {t}");
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["type"], "response.create");
        let placeholders: Vec<String> = redact::placeholder_regex()
            .find_iter(&t)
            .map(|m| m.as_str().to_string())
            .collect();
        let echo = placeholders.join(" ");
        let (a, b) = echo.split_at(echo.len() / 2);
        let ev = |d: String| {
            Message::text(
                serde_json::json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":d})
                    .to_string(),
            )
        };
        ws.send(ev(format!("Echo {a}"))).await.unwrap();
        ws.send(ev(format!("{b}!"))).await.unwrap();
        ws.send(Message::text(
            serde_json::json!({"type":"response.completed","response":{"model":"mock","status":"completed","usage":{"input_tokens":7,"output_tokens":2}}}).to_string(),
        ))
        .await
        .unwrap();
        let _ = ws.close(None).await;
    });
    Response::builder()
        .status(101)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-accept", accept)
        .body(Full::new(Bytes::new()).boxed())
        .unwrap()
}

#[derive(Default)]
struct CaptureSink(Mutex<Vec<RequestLog>>);
impl LogSink for CaptureSink {
    fn record(&self, log: RequestLog) {
        self.0.lock().unwrap().push(log);
    }
}

async fn start_proxy(upstream: SocketAddr, sink: Arc<CaptureSink>) -> SocketAddr {
    let redactor = Arc::new(Redactor::from_rules(&builtin_rules(), 1000).unwrap());
    start_proxy_with(upstream, sink, redactor).await
}

async fn start_proxy_with(upstream: SocketAddr, sink: Arc<CaptureSink>, redactor: Arc<Redactor>) -> SocketAddr {
    let dir = std::env::temp_dir().join(format!("pg-e2e-{}", uuid::Uuid::new_v4()));
    let ca = Arc::new(CertificateAuthority::load_or_create(&dir).unwrap());
    let mut cfg = ProxyConfig::default();
    cfg.listen = "127.0.0.1:0".parse().unwrap();
    cfg.reverse_routes = vec![ReverseRoute {
        prefix: "anthropic".into(),
        upstream: format!("http://{upstream}"),
    }];
    cfg.store_redacted_bodies = true;
    let (_proxy, handle) = proxy_core::start(cfg, ca, redactor, sink).await.unwrap();
    handle.local_addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn reverse_entry_redacts_and_restores_json() {
    let up = mock_upstream().await;
    let sink = Arc::new(CaptureSink::default());
    let proxy = start_proxy(up, sink.clone()).await;

    let body = serde_json::json!({
        "model":"claude-test","max_tokens":10,
        "system":"user key is ".to_string() + KEY,
        "messages":[{"role":"user","content":format!("contact {EMAIL} please")}]
    });
    let resp = client()
        .post(format!("http://{proxy}/anthropic/v1/messages"))
        .header("content-type", "application/json")
        .header("user-agent", "claude-cli/1.0")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains(EMAIL), "响应未还原邮箱: {text}");
    assert!(text.contains(KEY), "响应未还原密钥: {text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["usage"]["input_tokens"], 3);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let logs = sink.0.lock().unwrap();
    assert_eq!(logs.len(), 1);
    let l = &logs[0];
    assert_eq!(l.agent, "claude");
    assert_eq!(l.status, Some(200));
    assert_eq!(l.redaction_count(), 2);
    assert_eq!(l.restored_count, 2);
    assert_eq!(l.request_meta.as_ref().unwrap().model.as_deref(), Some("claude-test"));
    assert_eq!(l.usage.as_ref().unwrap().output_tokens, Some(4));
    let redacted = l.redacted_body.as_ref().unwrap();
    assert!(!redacted.contains(EMAIL));
    assert!(redacted.contains("PG_EMAIL_"));
    assert!(redacted.contains("PG_SECRET_"));
}

#[tokio::test]
async fn reverse_entry_restores_split_sse() {
    let up = mock_upstream().await;
    let sink = Arc::new(CaptureSink::default());
    let proxy = start_proxy(up, sink.clone()).await;

    let body = serde_json::json!({
        "model":"claude-test","stream":true,
        "messages":[{"role":"user","content":[{"type":"text","text":format!("mail {EMAIL}")}]}]
    });
    let resp = client()
        .post(format!("http://{proxy}/anthropic/v1/messages"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    // 占位符被上游拆在两个 SSE 事件里，但拼起来后仍应能还原
    let joined: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .filter_map(|v| v["delta"]["text"].as_str().map(str::to_string))
        .collect();
    assert!(joined.contains(EMAIL), "SSE 拼接后未还原: {joined}");
    assert!(!text.contains("PG_EMAIL_"), "响应仍含占位符: {text}");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let logs = sink.0.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].streamed);
    let u = logs[0].usage.as_ref().unwrap();
    assert_eq!(u.input_tokens, Some(11));
    assert_eq!(u.output_tokens, Some(5));
    assert_eq!(u.stop_reason.as_deref(), Some("end_turn"));
}

/// 隐身身份 + 拟真风格：上游看到的是假身份与假邮箱，SSE 回显（别名被拆到两个事件）仍能还原成真实值。
#[tokio::test]
async fn persona_alias_roundtrip_over_sse() {
    let up = mock_upstream().await;
    let sink = Arc::new(CaptureSink::default());
    let redactor = Arc::new(Redactor::from_rules(&builtin_rules(), 1000).unwrap());
    let identity = vec![
        redact::IdentityField {
            id: "name".into(),
            label: "姓名".into(),
            entity_type: "NAME".into(),
            real_values: vec!["王大锤".into()],
            alias: "陈小明".into(),
            case_insensitive: false,
            enabled: true,
        },
        redact::IdentityField {
            id: "org".into(),
            label: "公司".into(),
            entity_type: "ORG".into(),
            real_values: vec!["真实公司".into()],
            alias: "星河科技".into(),
            case_insensitive: false,
            enabled: true,
        },
    ];
    redactor
        .configure(&builtin_rules(), &identity, redact::Style::Synthetic)
        .unwrap();
    let proxy = start_proxy_with(up, sink.clone(), redactor).await;

    let original = format!("我是王大锤，在真实公司工作，邮箱 {EMAIL}");
    let body = serde_json::json!({
        "model":"claude-test","stream":true,"echo_all":true,
        "messages":[{"role":"user","content":[{"type":"text","text":original}]}]
    });
    let resp = client()
        .post(format!("http://{proxy}/anthropic/v1/messages"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let joined: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .filter_map(|v| v["delta"]["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(joined, format!("Your email is {original} ok"), "SSE 拼接后未完整还原: {text}");
    assert!(!text.contains("陈小明") && !text.contains("星河科技") && !text.contains("PG_"), "{text}");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let logs = sink.0.lock().unwrap();
    assert_eq!(logs.len(), 1);
    let ids: Vec<&str> = logs[0].redaction_events.iter().map(|r| r.rule_id.as_str()).collect();
    assert!(ids.contains(&"identity.name") && ids.contains(&"identity.org") && ids.contains(&"builtin.email"), "{ids:?}");
    // 请求体里不应再有真实值，也不应出现占位符（拟真风格）
    let stored = logs[0].redacted_body.as_deref().unwrap();
    assert!(stored.contains("陈小明") && stored.contains("星河科技"));
    assert!(!stored.contains(EMAIL) && !stored.contains("PG_EMAIL_"), "{stored}");
    assert_eq!(logs[0].restored_count, 3);
}

#[tokio::test]
async fn zstd_request_body_is_decoded_before_redaction() {
    let up = mock_upstream().await;
    let sink = Arc::new(CaptureSink::default());
    let proxy = start_proxy(up, sink.clone()).await;

    let body = serde_json::json!({
        "model":"claude-test","max_tokens":10,
        "messages":[{"role":"user","content":format!("contact {EMAIL} please")}]
    });
    let compressed = zstd::stream::encode_all(body.to_string().as_bytes(), 3).unwrap();
    let resp = client()
        .post(format!("http://{proxy}/anthropic/v1/messages"))
        .header("content-type", "application/json")
        .header("content-encoding", "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains(EMAIL), "响应未还原邮箱: {text}");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let logs = sink.0.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].redaction_count(), 1);
    assert!(logs[0].warnings.is_empty(), "{:?}", logs[0].warnings);
}

#[tokio::test]
async fn websocket_responses_are_redacted_and_restored() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;

    let up = mock_upstream().await;
    let sink = Arc::new(CaptureSink::default());
    let proxy = start_proxy(up, sink.clone()).await;

    // 手工完成到代理反向入口的 WebSocket 握手（Codex 在模式 A 下就是这样连的）
    let tcp = tokio::net::TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    let req = Request::builder()
        .uri("/anthropic/v1/responses")
        .header("host", proxy.to_string())
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-extensions", "permessage-deflate")
        .header("user-agent", "codex_cli_rs/0.1")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();
    let mut resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 101);
    assert!(resp.headers().get("sec-websocket-extensions").is_none());
    let upgraded = hyper::upgrade::on(&mut resp).await.unwrap();
    let mut ws =
        tokio_tungstenite::WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;

    let create = serde_json::json!({
        "type":"response.create","model":"gpt-test","stream":true,
        "input":[{"role":"user","content":[{"type":"input_text","text":format!("mail {EMAIL}")}]}]
    });
    ws.send(Message::text(create.to_string())).await.unwrap();

    let mut joined = String::new();
    let mut completed = false;
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if let Some(d) = v["delta"].as_str() {
                    joined.push_str(d);
                }
                if v["type"] == "response.completed" {
                    completed = true;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    assert!(completed);
    assert_eq!(joined, format!("Echo {EMAIL}!"), "WS 拼接后未还原");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let logs = sink.0.lock().unwrap();
    // 一条连接记录（GET 101）+ 一条模型调用记录（WS）
    let turn = logs.iter().find(|l| l.method == "WS").expect("缺少 WS 轮次日志");
    assert_eq!(turn.agent, "codex");
    assert_eq!(turn.redaction_count(), 1);
    assert_eq!(turn.restored_count, 1);
    assert_eq!(turn.status, Some(200));
    assert_eq!(turn.usage.as_ref().unwrap().input_tokens, Some(7));
    assert_eq!(turn.request_meta.as_ref().unwrap().model.as_deref(), Some("gpt-test"));
    let conn_log = logs.iter().find(|l| l.status == Some(101)).expect("缺少连接日志");
    assert!(conn_log.warnings.is_empty(), "{:?}", conn_log.warnings);
}

#[tokio::test]
async fn local_endpoints() {
    let up = mock_upstream().await;
    let proxy = start_proxy(up, Arc::new(CaptureSink::default())).await;
    let pac = client()
        .get(format!("http://{proxy}/proxy.pac"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(pac.contains("FindProxyForURL"));
    assert!(pac.contains(&format!("PROXY {proxy}")));
    let pem = client()
        .get(format!("http://{proxy}/pg/ca.pem"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));
}
