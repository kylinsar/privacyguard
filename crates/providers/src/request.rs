use crate::classify::ApiKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 从请求体中提取的、用于日志的元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestMeta {
    pub model: Option<String>,
    pub stream: bool,
    pub tool_names: Vec<String>,
    pub message_count: usize,
    /// 会话标识（Claude Code 的 metadata.user_id 中的 session_xxx / Codex 的 prompt_cache_key / OpenAI user）
    #[serde(default)]
    pub session_id: Option<String>,
    /// Responses API 的 reasoning.effort
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// 从请求体中推断会话 id。
fn session_from_body(api: ApiKind, body: &Value) -> Option<String> {
    match api {
        ApiKind::AnthropicMessages => {
            let uid = body.pointer("/metadata/user_id")?.as_str()?;
            // 形如 user_<hash>_account_<uuid>_session_<uuid>
            if let Some(pos) = uid.find("session_") {
                let s = &uid[pos + "session_".len()..];
                let s: String = s.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-').collect();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            Some(uid.chars().take(64).collect())
        }
        ApiKind::Responses => body
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(64).collect()),
        ApiKind::ChatCompletions => body
            .get("user")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(64).collect()),
        _ => None,
    }
}

pub fn request_meta(api: ApiKind, body: &Value) -> RequestMeta {
    let model = body.get("model").and_then(|v| v.as_str()).map(str::to_string);
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let tool_names = body
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    t.get("name")
                        .and_then(|n| n.as_str())
                        .or_else(|| t.pointer("/function/name").and_then(|n| n.as_str()))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let message_count = match api {
        ApiKind::Responses => body
            .get("input")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or_else(|| usize::from(body.get("input").is_some())),
        _ => body
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    };
    RequestMeta {
        model,
        stream,
        tool_names,
        message_count,
        session_id: session_from_body(api, body),
        reasoning_effort: body
            .pointer("/reasoning/effort")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// 对请求体中所有「用户内容」字段应用 `f`（原地修改）。返回被处理的字段数。
/// 请求帧字段（model、temperature、工具 schema、id 等）不动。
pub fn redact_request_body(api: ApiKind, body: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    match api {
        ApiKind::AnthropicMessages => anthropic(body, f),
        ApiKind::ChatCompletions => chat_completions(body, f),
        ApiKind::Responses => responses(body, f),
        ApiKind::Passthrough | ApiKind::Unknown => 0,
    }
}

fn apply_str(v: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    if let Value::String(s) = v {
        let new = f(s);
        if new != *s {
            *s = new;
        }
        1
    } else {
        0
    }
}

/// 递归处理所有字符串叶子（用于 tool_use.input 这类任意 JSON）。
fn apply_all_strings(v: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    match v {
        Value::String(_) => apply_str(v, f),
        Value::Array(a) => a.iter_mut().map(|x| apply_all_strings(x, f)).sum(),
        Value::Object(o) => o.values_mut().map(|x| apply_all_strings(x, f)).sum(),
        _ => 0,
    }
}

/// 处理 `string | [{type:"text", text}] ` 形式的内容。`text_keys` 指明块内文本字段名。
fn apply_content(v: &mut Value, text_keys: &[&str], f: &mut dyn FnMut(&str) -> String) -> usize {
    match v {
        Value::String(_) => apply_str(v, f),
        Value::Array(blocks) => {
            let mut n = 0;
            for b in blocks {
                if let Value::Object(o) = b {
                    for k in text_keys {
                        if let Some(t) = o.get_mut(*k) {
                            n += apply_str(t, f);
                        }
                    }
                }
            }
            n
        }
        _ => 0,
    }
}

fn anthropic(body: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    let mut n = 0;
    if let Some(sys) = body.get_mut("system") {
        n += apply_content(sys, &["text"], f);
    }
    if let Some(Value::Array(msgs)) = body.get_mut("messages") {
        for m in msgs {
            let Some(content) = m.get_mut("content") else { continue };
            match content {
                Value::String(_) => n += apply_str(content, f),
                Value::Array(blocks) => {
                    for b in blocks {
                        let Some(ty) = b.get("type").and_then(|t| t.as_str()).map(str::to_string) else {
                            continue;
                        };
                        match ty.as_str() {
                            "text" => {
                                if let Some(t) = b.get_mut("text") {
                                    n += apply_str(t, f);
                                }
                            }
                            "tool_use" => {
                                if let Some(input) = b.get_mut("input") {
                                    n += apply_all_strings(input, f);
                                }
                            }
                            "tool_result" => {
                                if let Some(c) = b.get_mut("content") {
                                    n += apply_content(c, &["text"], f);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    n
}

fn chat_completions(body: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    let mut n = 0;
    if let Some(Value::Array(msgs)) = body.get_mut("messages") {
        for m in msgs {
            if let Some(c) = m.get_mut("content") {
                n += apply_content(c, &["text"], f);
            }
            if let Some(Value::Array(calls)) = m.get_mut("tool_calls") {
                for c in calls {
                    if let Some(args) = c.pointer_mut("/function/arguments") {
                        n += apply_str(args, f);
                    }
                }
            }
        }
    }
    n
}

fn responses(body: &mut Value, f: &mut dyn FnMut(&str) -> String) -> usize {
    let mut n = 0;
    if let Some(ins) = body.get_mut("instructions") {
        n += apply_str(ins, f);
    }
    let Some(input) = body.get_mut("input") else { return n };
    match input {
        Value::String(_) => n += apply_str(input, f),
        Value::Array(items) => {
            for item in items {
                let ty = item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("message")
                    .to_string();
                match ty.as_str() {
                    "message" => {
                        if let Some(c) = item.get_mut("content") {
                            n += apply_content(c, &["text"], f);
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        if let Some(a) = item.get_mut("arguments") {
                            n += apply_str(a, f);
                        }
                        if let Some(a) = item.get_mut("input") {
                            n += apply_str(a, f);
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        if let Some(o) = item.get_mut("output") {
                            n += apply_content(o, &["text"], f);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upper(s: &str) -> String {
        s.to_uppercase()
    }

    #[test]
    fn anthropic_fields() {
        let mut body = json!({
            "model": "claude",
            "system": [{"type":"text","text":"sys"}],
            "messages": [
                {"role":"user","content":"hello"},
                {"role":"assistant","content":[{"type":"tool_use","id":"t","name":"read","input":{"path":"a.txt","nested":{"k":"v"}}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"file body"}]}]}
            ]
        });
        let n = redact_request_body(ApiKind::AnthropicMessages, &mut body, &mut upper);
        assert_eq!(n, 5);
        assert_eq!(body["system"][0]["text"], "SYS");
        assert_eq!(body["messages"][0]["content"], "HELLO");
        assert_eq!(body["messages"][1]["content"][0]["input"]["nested"]["k"], "V");
        assert_eq!(body["messages"][1]["content"][0]["name"], "read");
        assert_eq!(body["messages"][2]["content"][0]["content"][0]["text"], "FILE BODY");
        assert_eq!(body["model"], "claude");
    }

    #[test]
    fn responses_fields() {
        let mut body = json!({
            "model":"gpt", "instructions":"be nice", "stream": true,
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
                {"type":"function_call","call_id":"c","name":"sh","arguments":"{\"cmd\":\"ls\"}"},
                {"type":"function_call_output","call_id":"c","output":"a b c"}
            ],
            "tools":[{"type":"function","name":"sh"}]
        });
        let n = redact_request_body(ApiKind::Responses, &mut body, &mut upper);
        assert_eq!(n, 4);
        assert_eq!(body["instructions"], "BE NICE");
        assert_eq!(body["input"][2]["output"], "A B C");
        let meta = request_meta(ApiKind::Responses, &body);
        assert_eq!(meta.model.as_deref(), Some("gpt"));
        assert!(meta.stream);
        assert_eq!(meta.tool_names, vec!["sh"]);
        assert_eq!(meta.message_count, 3);
    }
}
