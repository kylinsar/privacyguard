//! SSE 感知的占位符还原器。
//!
//! 模型流式输出时，一个占位符（如 `PG_EMAIL_7a236414`）往往被拆到多个 delta 事件里，
//! 每个事件是独立 JSON，纯文本级替换无法跨事件还原。本模块：
//! 1. 按事件切分 SSE 流并解析 `data:` JSON；
//! 2. 通过 `providers::sse::carriers` 找到承载模型文本的字段；
//! 3. 对同一文本流（key）跨事件缓存「可能是占位符前缀」的尾部，拼到下一事件开头再还原；
//! 4. 收到块结束信号或流结束时，用上一事件作模板补发缓存的尾部。

use parking_lot::Mutex;
use providers::sse::{carriers, terminator, Terminator};
use providers::ApiKind;
use redact::PlaceholderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

struct Pending {
    tail: String,
    template: Value,
    pointer: String,
    event_name: Option<String>,
}

/// 一个待输出的事件（已还原）。SSE 与 WebSocket 两种载体共用。
#[derive(Debug, Clone)]
pub struct OutEvent {
    pub event_name: Option<String>,
    pub data: Value,
}

pub struct SseRestorer {
    api: ApiKind,
    map: Arc<Mutex<PlaceholderMap>>,
    buf: Vec<u8>,
    pending: HashMap<String, Pending>,
    restored: usize,
}

impl SseRestorer {
    pub fn new(api: ApiKind, map: Arc<Mutex<PlaceholderMap>>) -> Self {
        Self {
            api,
            map,
            buf: Vec::new(),
            pending: HashMap::new(),
            restored: 0,
        }
    }

    pub fn restored_count(&self) -> usize {
        self.restored
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some((end, sep_len)) = find_event_boundary(&self.buf) else { break };
            let event: Vec<u8> = self.buf.drain(..end + sep_len).collect();
            let body = &event[..end];
            out.extend(self.process_event(body));
        }
        out
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            if rest.iter().any(|b| !b.is_ascii_whitespace()) {
                out.extend(self.process_event(&rest));
            }
        }
        let tail = self.flush_all();
        out.extend(self.format_events(&tail));
        out
    }

    fn process_event(&mut self, raw: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(raw) else {
            let mut v = raw.to_vec();
            v.extend_from_slice(b"\n\n");
            return v;
        };
        let mut event_name: Option<String> = None;
        let mut data_lines: Vec<&str> = Vec::new();
        let mut other_lines: Vec<&str> = Vec::new();
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(v) = line.strip_prefix("event:") {
                event_name = Some(v.trim_start().to_string());
            } else if let Some(v) = line.strip_prefix("data:") {
                data_lines.push(v.strip_prefix(' ').unwrap_or(v));
            } else if !line.is_empty() {
                other_lines.push(line);
            }
        }
        if data_lines.is_empty() {
            let mut v = raw.to_vec();
            v.extend_from_slice(b"\n\n");
            return v;
        }
        let data = data_lines.join("\n");

        let mut out = Vec::new();
        let parsed: Option<Value> = serde_json::from_str(&data).ok();
        let Some(json) = parsed else {
            // 非 JSON（如 OpenAI 的 [DONE]）：先补发所有缓存尾部
            if data.trim() == "[DONE]" {
                let flushed = self.flush_pending(|_| true);
                out.extend(self.format_events(&flushed));
            }
            out.extend(emit(&other_lines, event_name.as_deref(), &data));
            return out;
        };

        let events = self.restore_json_event(json, event_name);
        // 首个事件（若是补发的尾部）沿用模板事件名；本事件带上原始的其他行
        let last = events.len().saturating_sub(1);
        for (i, ev) in events.iter().enumerate() {
            let other: &[&str] = if i == last { &other_lines } else { &[] };
            out.extend(emit(other, ev.event_name.as_deref(), &ev.data.to_string()));
        }
        out
    }

    fn format_events(&self, events: &[OutEvent]) -> Vec<u8> {
        let mut out = Vec::new();
        for ev in events {
            out.extend(emit(&[], ev.event_name.as_deref(), &ev.data.to_string()));
        }
        out
    }

    /// 处理一个 JSON 事件，返回应按顺序输出的事件列表（可能包含补发的缓存尾部，
    /// 最后一个是本事件本身）。WebSocket 载体直接调用此方法。
    pub fn restore_json_event(&mut self, mut json: Value, event_name: Option<String>) -> Vec<OutEvent> {
        let cs = carriers(self.api, &json);
        let mut out = Vec::new();
        if cs.is_empty() {
            // 完整事件：文本级还原即可（在序列化文本上替换，保证 JSON 转义正确）
            let raw = json.to_string();
            let (restored, n) = self.map.lock().restore_text(&raw, true);
            self.restored += n;
            let data = if n > 0 {
                serde_json::from_str(&restored).unwrap_or(json)
            } else {
                json
            };
            // 终止信号：先补发对应尾部，再发本事件
            out.extend(self.apply_terminator(&data));
            out.push(OutEvent { event_name, data });
            return out;
        }

        for c in cs {
            let Some(cur) = json.pointer(&c.pointer).and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            let mut combined = self.pending.remove(&c.key).map(|p| p.tail).unwrap_or_default();
            combined.push_str(&cur);
            let (restored, n, hold) = {
                let mut map = self.map.lock();
                let (restored, n) = map.restore_text(&combined, false);
                let hold = map.holdback(restored.as_bytes());
                (restored, n, hold)
            };
            self.restored += n;
            let split = restored.len() - hold;
            let (emit_text, tail) = restored.split_at(split);
            if let Some(slot) = json.pointer_mut(&c.pointer) {
                *slot = Value::String(emit_text.to_string());
            }
            if hold > 0 {
                self.pending.insert(
                    c.key.clone(),
                    Pending {
                        tail: tail.to_string(),
                        template: json.clone(),
                        pointer: c.pointer.clone(),
                        event_name: event_name.clone(),
                    },
                );
            }
        }
        out.extend(self.apply_terminator(&json));
        out.push(OutEvent { event_name, data: json });
        out
    }

    /// 流结束：补发所有缓存尾部。
    pub fn flush_all(&mut self) -> Vec<OutEvent> {
        self.flush_pending(|_| true)
    }

    fn apply_terminator(&mut self, json: &Value) -> Vec<OutEvent> {
        match terminator(self.api, json) {
            Terminator::None => Vec::new(),
            Terminator::All => self.flush_pending(|_| true),
            Terminator::Prefix(p) => self.flush_pending(|k| k.starts_with(&p)),
        }
    }

    /// 把满足条件的缓存尾部用模板事件补发出去。
    fn flush_pending(&mut self, pred: impl Fn(&str) -> bool) -> Vec<OutEvent> {
        let keys: Vec<String> = self.pending.keys().filter(|k| pred(k)).cloned().collect();
        let mut out = Vec::new();
        for k in keys {
            let Some(mut p) = self.pending.remove(&k) else { continue };
            if p.tail.is_empty() {
                continue;
            }
            let (restored, n) = self.map.lock().restore_text(&p.tail, false);
            self.restored += n;
            if let Some(slot) = p.template.pointer_mut(&p.pointer) {
                *slot = Value::String(restored);
            }
            out.push(OutEvent {
                event_name: p.event_name,
                data: p.template,
            });
        }
        out
    }
}

fn emit(other: &[&str], event_name: Option<&str>, data: &str) -> Vec<u8> {
    let mut s = String::new();
    for l in other {
        s.push_str(l);
        s.push('\n');
    }
    if let Some(e) = event_name {
        s.push_str("event: ");
        s.push_str(e);
        s.push('\n');
    }
    for line in data.split('\n') {
        s.push_str("data: ");
        s.push_str(line);
        s.push('\n');
    }
    s.push('\n');
    s.into_bytes()
}

/// 找到第一个事件边界（`\n\n` 或 `\r\n\r\n`），返回 (边界起点, 分隔符长度)。
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Mutex<PlaceholderMap>>, String) {
        let mut m = PlaceholderMap::with_key([9u8; 32], 100);
        let p = m.placeholder_for("alice@corp.com", "EMAIL");
        (Arc::new(Mutex::new(m)), p)
    }

    fn anthropic_delta(text: &str) -> String {
        let j = serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}});
        format!("event: content_block_delta\ndata: {j}\n\n")
    }

    fn collect_text(out: &[u8]) -> String {
        String::from_utf8_lossy(out)
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .filter_map(|v| v.pointer("/delta/text").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    #[test]
    fn placeholder_split_across_events() {
        let (map, p) = setup();
        // 模拟模型把占位符拆成多个 token 输出
        let pieces = ["Mail: PG", "_EM", "AIL_", &p[9..12], &p[12..], " done"];
        let mut r = SseRestorer::new(ApiKind::AnthropicMessages, map);
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(r.feed(anthropic_delta(piece).as_bytes()));
        }
        out.extend(r.feed(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.extend(r.finish());
        let text = collect_text(&out);
        assert_eq!(text, "Mail: alice@corp.com done");
        assert_eq!(r.restored_count(), 1);
        // 事件仍然是合法 SSE，且 stop 事件在最后
        let s = String::from_utf8(out).unwrap();
        assert!(s.trim_end().ends_with("\"index\":0}"));
    }

    #[test]
    fn false_prefix_is_flushed_on_stop() {
        let (map, _) = setup();
        let mut r = SseRestorer::new(ApiKind::AnthropicMessages, map);
        let mut out = Vec::new();
        out.extend(r.feed(anthropic_delta("see PG_").as_bytes()));
        out.extend(r.feed(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        assert_eq!(collect_text(&out), "see PG_");
    }

    #[test]
    fn tcp_chunking_does_not_matter() {
        let (map, p) = setup();
        let stream = format!(
            "{}{}event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
            anthropic_delta(&format!("a {}", &p[..5])),
            anthropic_delta(&format!("{} b", &p[5..]))
        );
        for cut in 1..stream.len() {
            let mut r = SseRestorer::new(ApiKind::AnthropicMessages, map.clone());
            let mut out = r.feed(&stream.as_bytes()[..cut]);
            out.extend(r.feed(&stream.as_bytes()[cut..]));
            out.extend(r.finish());
            assert_eq!(collect_text(&out), "a alice@corp.com b", "cut={cut}");
        }
    }

    #[test]
    fn identity_alias_split_across_events() {
        let mut m = PlaceholderMap::with_key([9u8; 32], 100);
        m.set_identity(&[redact::IdentityField {
            id: "name".into(),
            entity_type: "NAME".into(),
            real_values: vec!["王大锤".into()],
            alias: "陈小明".into(),
            ..Default::default()
        }]);
        assert_eq!(m.placeholder_for("王大锤", "NAME"), "陈小明");
        let map = Arc::new(Mutex::new(m));
        let mut r = SseRestorer::new(ApiKind::AnthropicMessages, map);
        let mut out = Vec::new();
        for piece in ["你好，陈", "小", "明先生"] {
            out.extend(r.feed(anthropic_delta(piece).as_bytes()));
        }
        out.extend(r.feed(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        assert_eq!(collect_text(&out), "你好，王大锤先生");
        assert_eq!(r.restored_count(), 1);
    }

    #[test]
    fn responses_api_delta() {
        let (map, p) = setup();
        let mut r = SseRestorer::new(ApiKind::Responses, map);
        let ev = |d: &str| {
            let j = serde_json::json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":d});
            format!("event: response.output_text.delta\ndata: {j}\n\n")
        };
        let mut out = Vec::new();
        out.extend(r.feed(ev(&p[..4]).as_bytes()));
        out.extend(r.feed(ev(&p[4..]).as_bytes()));
        out.extend(r.finish());
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("alice@corp.com"));
        assert!(!s.contains("PG_EMAIL"));
    }
}
