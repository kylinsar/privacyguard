//! WebSocket 消息级脱敏 / 还原。
//!
//! Codex 在 ChatGPT 登录方式下默认走 Responses API 的 WebSocket 模式
//! （`wss://chatgpt.com/backend-api/codex/responses`）：客户端每轮发送一条
//! `{"type":"response.create", "input":[...], ...}` 文本帧，服务端把与 SSE 完全相同的事件
//! 逐条以文本帧推回。因此：
//! - 客户端 -> 服务端：把消息当作 Responses 请求体做脱敏；
//! - 服务端 -> 客户端：复用 `SseRestorer` 的 JSON 事件级还原（含跨帧占位符缓存）。
//!
//! 每个 `response.create` 记为一条模型调用记录，收到 `response.completed/failed/incomplete`
//! 或 `error` 时落库。

use crate::body::LogFinalizer;
use crate::log::{LogSink, RequestLog};
use crate::pipeline::MAX_DEBUG_BODY;
use crate::sse_restore::SseRestorer;
use futures_util::{SinkExt, StreamExt};
use providers::ApiKind;
use redact::{RedactionEvent, Redactor};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// 一条 WebSocket 连接上的脱敏会话状态。
pub struct WsSession {
    api: ApiKind,
    redactor: Arc<Redactor>,
    restorer: SseRestorer,
    sink: Arc<dyn LogSink>,
    /// 每轮日志的模板（entry / agent / host / path 等）
    template: RequestLog,
    current: Option<LogFinalizer>,
    store_bodies: bool,
    /// 是否还原模型输出中的占位符
    restore: bool,
}

impl WsSession {
    pub fn new(
        api: ApiKind,
        redactor: Arc<Redactor>,
        sink: Arc<dyn LogSink>,
        template: RequestLog,
        store_bodies: bool,
        restore: bool,
    ) -> Self {
        let restorer = SseRestorer::new(api, redactor.map());
        Self {
            api,
            redactor,
            restorer,
            sink,
            template,
            current: None,
            store_bodies,
            restore,
        }
    }

    /// 客户端发来的文本帧。返回应发给上游的文本。
    pub fn on_client_text(&mut self, text: &str) -> String {
        let Ok(mut json) = serde_json::from_str::<Value>(text) else {
            return text.to_string();
        };
        if !is_request(&json) {
            return text.to_string();
        }

        // 新一轮：先结束上一轮（服务端可能没发终止事件）
        self.finish_turn(None);
        let mut log = self.template.clone();
        log.id = uuid::Uuid::new_v4().to_string();
        log.started_at = chrono::Utc::now();
        log.method = "WS".into();
        log.req_bytes = text.len() as u64;
        log.streamed = true;

        let meta = providers::request_meta(self.api, &json);
        if log.session_id.is_none() {
            log.session_id = meta.session_id.clone();
        }
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        let redactor = self.redactor.clone();
        let mut f = |s: &str| -> String {
            let out = redactor.redact_text(s);
            for e in out.events {
                *counts.entry((e.rule_id, e.entity_type)).or_insert(0) += e.count;
            }
            out.text
        };
        providers::redact_request_body(self.api, &mut json, &mut f);
        log.request_meta = Some(meta);
        log.redaction_events = counts
            .into_iter()
            .map(|((rule_id, entity_type), count)| RedactionEvent {
                rule_id,
                entity_type,
                count,
            })
            .collect();
        let out = json.to_string();
        if self.store_bodies {
            log.redacted_body = Some(out.chars().take(MAX_DEBUG_BODY).collect());
        }
        self.current = Some(LogFinalizer::new(log, Instant::now(), self.sink.clone()));
        out
    }

    /// 服务端发来的文本帧。返回应按顺序发给客户端的文本列表。
    pub fn on_server_text(&mut self, text: &str) -> Vec<String> {
        let Ok(json) = serde_json::from_str::<Value>(text) else {
            if !self.restore {
                return vec![text.to_string()];
            }
            let (restored, n) = self.redactor.restore_text(text, false);
            self.bump_restored(n);
            return vec![restored];
        };
        let usage = providers::parse_usage_json(self.api, &json);
        let terminal = is_terminal(&json);
        let out: Vec<String> = if self.restore {
            self.restorer
                .restore_json_event(json, None)
                .into_iter()
                .map(|e| e.data.to_string())
                .collect()
        } else {
            vec![text.to_string()]
        };
        if let Some(cur) = &mut self.current {
            cur.mark_first_byte();
            let log = cur.log_mut();
            log.resp_bytes += out.iter().map(|s| s.len() as u64).sum::<u64>();
            log.restored_count = self.restorer.restored_count();
            if !usage.is_empty() {
                let mut u = log.usage.take().unwrap_or_default();
                u.merge(usage);
                log.usage = Some(u);
            }
        }
        if terminal {
            self.finish_turn(None);
        }
        out
    }

    /// 连接结束。
    pub fn on_close(&mut self, error: Option<String>) {
        // 补发缓存尾部已无意义（连接已关），只结束日志
        let _ = self.restorer.flush_all();
        self.finish_turn(error);
    }

    fn bump_restored(&mut self, n: usize) {
        if let Some(cur) = &mut self.current {
            cur.log_mut().restored_count += n;
        }
    }

    fn finish_turn(&mut self, error: Option<String>) {
        if let Some(mut cur) = self.current.take() {
            let log = cur.log_mut();
            if let Some(e) = error {
                log.error = Some(e);
            }
            if log.status.is_none() {
                log.status = Some(if log.error.is_some() { 502 } else { 200 });
            }
            if let Some(u) = &log.usage {
                if let Some(err) = &u.error {
                    log.error = Some(err.clone());
                }
            }
            cur.finish();
        }
    }
}

/// 客户端消息是否是一次模型请求（Responses WS：`response.create`；兼容直接发请求体的实现）。
fn is_request(v: &Value) -> bool {
    match v.get("type").and_then(|t| t.as_str()) {
        Some(t) => t == "response.create",
        None => v.get("input").is_some() || v.get("messages").is_some(),
    }
}

fn is_terminal(v: &Value) -> bool {
    matches!(
        v.get("type").and_then(|t| t.as_str()),
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    )
}

/// 在两条已升级的连接之间做消息级中继，直到任一方关闭。返回 (客户端->上游字节数, 上游->客户端字节数)。
pub async fn relay<D, U>(down: D, up: U, mut session: WsSession) -> (u64, u64)
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let down_ws = WebSocketStream::from_raw_socket(down, Role::Server, None).await;
    let up_ws = WebSocketStream::from_raw_socket(up, Role::Client, None).await;
    let (mut down_tx, mut down_rx) = down_ws.split();
    let (mut up_tx, mut up_rx) = up_ws.split();
    let (mut c2s, mut s2c) = (0u64, 0u64);
    let mut error: Option<String> = None;

    loop {
        tokio::select! {
            m = down_rx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let out = session.on_client_text(&t);
                    c2s += out.len() as u64;
                    if up_tx.send(Message::text(out)).await.is_err() { break; }
                }
                Some(Ok(Message::Close(f))) => {
                    let _ = up_tx.send(Message::Close(f)).await;
                    break;
                }
                Some(Ok(msg)) => {
                    c2s += msg.len() as u64;
                    if up_tx.send(msg).await.is_err() { break; }
                }
                Some(Err(e)) => { error = Some(format!("客户端 WebSocket 出错: {e}")); break; }
                None => break,
            },
            m = up_rx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let mut failed = false;
                    for out in session.on_server_text(&t) {
                        s2c += out.len() as u64;
                        if down_tx.send(Message::text(out)).await.is_err() { failed = true; break; }
                    }
                    if failed { break; }
                }
                Some(Ok(Message::Close(f))) => {
                    let _ = down_tx.send(Message::Close(f)).await;
                    break;
                }
                Some(Ok(msg)) => {
                    s2c += msg.len() as u64;
                    if down_tx.send(msg).await.is_err() { break; }
                }
                Some(Err(e)) => { error = Some(format!("上游 WebSocket 出错: {e}")); break; }
                None => break,
            },
        }
    }
    let _ = down_tx.close().await;
    let _ = up_tx.close().await;
    session.on_close(error);
    (c2s, s2c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::NoopSink;
    use redact::builtin_rules;

    fn session() -> WsSession {
        let redactor = Arc::new(Redactor::from_rules(&builtin_rules(), 100).unwrap());
        WsSession::new(
            ApiKind::Responses,
            redactor,
            Arc::new(NoopSink),
            RequestLog::default(),
            false,
            true,
        )
    }

    #[test]
    fn request_is_redacted_and_reply_restored() {
        let mut s = session();
        let req = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5",
            "input": [{"role":"user","content":[{"type":"input_text","text":"call me at +86 138 0013 8000 or a@b.io"}]}]
        });
        let out = s.on_client_text(&req.to_string());
        assert!(!out.contains("138 0013 8000"), "{out}");
        assert!(!out.contains("a@b.io"));
        let v: Value = serde_json::from_str(&out).unwrap();
        let sent = v["input"][0]["content"][0]["text"].as_str().unwrap().to_string();
        let ph_phone = sent.split_whitespace().find(|w| w.starts_with("PG_PHONE_")).unwrap().to_string();

        // 服务端把占位符拆到两帧
        let (a, b) = ph_phone.split_at(6);
        let ev = |d: &str| serde_json::json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":d}).to_string();
        let mut got = String::new();
        for frame in [ev(&format!("Sure: {a}")), ev(&format!("{b} ok")), serde_json::json!({"type":"response.output_text.done","output_index":0,"content_index":0,"text":ph_phone}).to_string()] {
            for o in s.on_server_text(&frame) {
                let v: Value = serde_json::from_str(&o).unwrap();
                if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                    got.push_str(d);
                }
                if let Some(t) = v.get("text").and_then(|d| d.as_str()) {
                    assert_eq!(t, "+86 138 0013 8000");
                }
            }
        }
        assert_eq!(got, "Sure: +86 138 0013 8000 ok");
        let done = serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":3,"output_tokens":4}}});
        s.on_server_text(&done.to_string());
        assert!(s.current.is_none());
    }

    #[test]
    fn restore_disabled_keeps_placeholders() {
        let redactor = Arc::new(Redactor::from_rules(&builtin_rules(), 100).unwrap());
        let mut s = WsSession::new(
            ApiKind::Responses,
            redactor,
            Arc::new(NoopSink),
            RequestLog::default(),
            false,
            false,
        );
        let req = serde_json::json!({"type":"response.create","input":"mail a@b.io"});
        let out = s.on_client_text(&req.to_string());
        assert!(!out.contains("a@b.io"));
        let ph = redact::placeholder_regex().find(&out).unwrap().as_str().to_string();
        let ev = serde_json::json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":ph}).to_string();
        let back = s.on_server_text(&ev);
        assert_eq!(back, vec![ev]);
    }

    #[test]
    fn non_request_frames_pass_through() {
        let mut s = session();
        let ping = r#"{"type":"response.cancel"}"#;
        assert_eq!(s.on_client_text(ping), ping);
        assert_eq!(s.on_client_text("not json"), "not json");
    }
}
