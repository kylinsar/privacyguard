use crate::classify::ApiKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageMeta {
    pub response_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub stop_reason: Option<String>,
    /// 上游返回的错误信息（若有）
    pub error: Option<String>,
    /// 推理（思考）token 数
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    /// 模型在本次响应中实际发起的工具调用名（按出现顺序，可重复）
    #[serde(default)]
    pub tool_calls: Vec<String>,
}

impl UsageMeta {
    /// 用 `other` 中非空字段覆盖自身。
    pub fn merge(&mut self, other: UsageMeta) {
        macro_rules! take {
            ($f:ident) => {
                if other.$f.is_some() {
                    self.$f = other.$f;
                }
            };
        }
        take!(response_model);
        take!(input_tokens);
        take!(output_tokens);
        take!(cache_read_tokens);
        take!(cache_write_tokens);
        take!(stop_reason);
        take!(error);
        take!(reasoning_tokens);
        self.tool_calls.extend(other.tool_calls);
    }

    pub fn is_empty(&self) -> bool {
        *self == UsageMeta::default()
    }
}

fn u64_at(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(|x| x.as_u64())
}
fn str_at(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(|x| x.as_str()).map(str::to_string)
}

/// 解析非流式响应体（或单个 SSE 事件的 JSON）。
pub fn parse_usage_json(api: ApiKind, v: &Value) -> UsageMeta {
    let mut m = UsageMeta::default();
    if let Some(err) = v.get("error") {
        m.error = err
            .get("message")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| Some(err.to_string()));
    }
    match api {
        ApiKind::AnthropicMessages => {
            // 非流式 message 或 SSE 的 message_start.message / message_delta
            let msg = if v.get("type").and_then(|t| t.as_str()) == Some("message_start") {
                v.get("message").unwrap_or(v)
            } else {
                v
            };
            m.response_model = str_at(msg, "/model");
            m.input_tokens = u64_at(msg, "/usage/input_tokens");
            m.output_tokens = u64_at(msg, "/usage/output_tokens").or(u64_at(v, "/usage/output_tokens"));
            m.cache_read_tokens = u64_at(msg, "/usage/cache_read_input_tokens");
            m.cache_write_tokens = u64_at(msg, "/usage/cache_creation_input_tokens");
            m.stop_reason = str_at(msg, "/stop_reason").or(str_at(v, "/delta/stop_reason"));
            // 工具调用：非流式 content[]；流式 content_block_start.content_block
            if v.get("type").and_then(|t| t.as_str()) == Some("content_block_start") {
                if let Some(name) = tool_name_anthropic(v.get("content_block").unwrap_or(&Value::Null)) {
                    m.tool_calls.push(name);
                }
            } else if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                m.tool_calls.extend(content.iter().filter_map(tool_name_anthropic));
            }
        }
        ApiKind::ChatCompletions => {
            m.response_model = str_at(v, "/model");
            m.input_tokens = u64_at(v, "/usage/prompt_tokens");
            m.output_tokens = u64_at(v, "/usage/completion_tokens");
            m.cache_read_tokens = u64_at(v, "/usage/prompt_tokens_details/cached_tokens");
            m.reasoning_tokens = u64_at(v, "/usage/completion_tokens_details/reasoning_tokens");
            m.stop_reason = str_at(v, "/choices/0/finish_reason");
            // 非流式 message.tool_calls；流式 delta.tool_calls（首个分片带 name）
            let calls = v
                .pointer("/choices/0/message/tool_calls")
                .or_else(|| v.pointer("/choices/0/delta/tool_calls"))
                .and_then(|c| c.as_array());
            if let Some(calls) = calls {
                m.tool_calls.extend(
                    calls
                        .iter()
                        .filter_map(|c| c.pointer("/function/name").and_then(|n| n.as_str()))
                        .filter(|n| !n.is_empty())
                        .map(str::to_string),
                );
            }
        }
        ApiKind::Responses => {
            // 非流式：顶层就是 response；流式：response.completed 等事件包一层 response
            let r = v.get("response").unwrap_or(v);
            m.response_model = str_at(r, "/model");
            m.input_tokens = u64_at(r, "/usage/input_tokens");
            m.output_tokens = u64_at(r, "/usage/output_tokens");
            m.cache_read_tokens = u64_at(r, "/usage/input_tokens_details/cached_tokens");
            m.reasoning_tokens = u64_at(r, "/usage/output_tokens_details/reasoning_tokens");
            m.stop_reason = str_at(r, "/status").or(str_at(r, "/incomplete_details/reason"));
            match v.get("type").and_then(|t| t.as_str()) {
                // 流式：每个输出项开始时记一次，避免与 response.completed 里的 output[] 重复
                Some("response.output_item.added") => {
                    if let Some(name) = tool_name_responses(v.get("item").unwrap_or(&Value::Null)) {
                        m.tool_calls.push(name);
                    }
                }
                Some(_) => {}
                // 非流式：顶层 output[]
                None => {
                    if let Some(out) = r.get("output").and_then(|o| o.as_array()) {
                        m.tool_calls.extend(out.iter().filter_map(tool_name_responses));
                    }
                }
            }
        }
        _ => {}
    }
    m
}

fn tool_name_anthropic(block: &Value) -> Option<String> {
    match block.get("type").and_then(|t| t.as_str())? {
        "tool_use" | "server_tool_use" | "mcp_tool_use" => {
            block.get("name").and_then(|n| n.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

fn tool_name_responses(item: &Value) -> Option<String> {
    let ty = item.get("type").and_then(|t| t.as_str())?;
    match ty {
        "function_call" | "custom_tool_call" | "mcp_call" => {
            item.get("name").and_then(|n| n.as_str()).map(str::to_string)
        }
        "local_shell_call" => Some("local_shell".into()),
        "apply_patch_call" => Some("apply_patch".into()),
        "web_search_call" => Some("web_search".into()),
        "file_search_call" => Some("file_search".into()),
        "computer_call" => Some("computer".into()),
        "code_interpreter_call" => Some("code_interpreter".into()),
        "image_generation_call" => Some("image_generation".into()),
        _ => None,
    }
}

/// 增量解析 SSE 流中的 `data:` 行，累积用量。对流量做旁路观察，不修改内容。
pub struct SseUsageCollector {
    api: ApiKind,
    buf: Vec<u8>,
    meta: UsageMeta,
}

impl SseUsageCollector {
    pub fn new(api: ApiKind) -> Self {
        Self {
            api,
            buf: Vec::new(),
            meta: UsageMeta::default(),
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            self.handle_line(&line);
        }
        // 防御：极长的行不再累积
        if self.buf.len() > 4 * 1024 * 1024 {
            self.buf.clear();
        }
    }

    fn handle_line(&mut self, line: &[u8]) {
        let Ok(s) = std::str::from_utf8(line) else { return };
        let s = s.trim_end_matches(['\r', '\n']);
        let Some(data) = s.strip_prefix("data:") else { return };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        if let Ok(v) = serde_json::from_str::<Value>(data) {
            let m = parse_usage_json(self.api, &v);
            self.meta.merge(m);
        }
    }

    pub fn finish(mut self) -> UsageMeta {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.handle_line(&rest);
        }
        self.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_stream_usage() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-x\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":3}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        );
        let mut c = SseUsageCollector::new(ApiKind::AnthropicMessages);
        for chunk in sse.as_bytes().chunks(13) {
            c.feed(chunk);
        }
        let m = c.finish();
        assert_eq!(m.response_model.as_deref(), Some("claude-x"));
        assert_eq!(m.input_tokens, Some(10));
        assert_eq!(m.output_tokens, Some(7));
        assert_eq!(m.cache_read_tokens, Some(3));
        assert_eq!(m.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn tool_calls_and_reasoning() {
        // Anthropic 流式 tool_use
        let v: Value = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"Bash","input":{}}}"#,
        )
        .unwrap();
        assert_eq!(parse_usage_json(ApiKind::AnthropicMessages, &v).tool_calls, vec!["Bash"]);
        // Anthropic 非流式
        let v: Value = serde_json::from_str(
            r#"{"type":"message","content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Read","input":{}}],"usage":{"input_tokens":1,"output_tokens":2}}"#,
        )
        .unwrap();
        assert_eq!(parse_usage_json(ApiKind::AnthropicMessages, &v).tool_calls, vec!["Read"]);
        // Responses 流式 output_item.added + completed（后者不重复计）
        let added: Value = serde_json::from_str(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","name":"shell","arguments":""}}"#,
        )
        .unwrap();
        assert_eq!(parse_usage_json(ApiKind::Responses, &added).tool_calls, vec!["shell"]);
        let done: Value = serde_json::from_str(
            r#"{"type":"response.completed","response":{"output":[{"type":"function_call","name":"shell"}],"usage":{"input_tokens":1,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":4}}}}"#,
        )
        .unwrap();
        let m = parse_usage_json(ApiKind::Responses, &done);
        assert!(m.tool_calls.is_empty());
        assert_eq!(m.reasoning_tokens, Some(4));
        // Responses 非流式
        let plain: Value = serde_json::from_str(
            r#"{"id":"r","output":[{"type":"local_shell_call"},{"type":"function_call","name":"f"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
        )
        .unwrap();
        assert_eq!(parse_usage_json(ApiKind::Responses, &plain).tool_calls, vec!["local_shell", "f"]);
        // ChatCompletions 流式 delta
        let d: Value = serde_json::from_str(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":""}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(parse_usage_json(ApiKind::ChatCompletions, &d).tool_calls, vec!["get_weather"]);
        let d2: Value = serde_json::from_str(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}"#,
        )
        .unwrap();
        assert!(parse_usage_json(ApiKind::ChatCompletions, &d2).tool_calls.is_empty());
    }

    #[test]
    fn responses_completed() {
        let v: Value = serde_json::from_str(
            r#"{"type":"response.completed","response":{"model":"gpt-5","status":"completed","usage":{"input_tokens":5,"output_tokens":9,"input_tokens_details":{"cached_tokens":2}}}}"#,
        )
        .unwrap();
        let m = parse_usage_json(ApiKind::Responses, &v);
        assert_eq!(m.input_tokens, Some(5));
        assert_eq!(m.cache_read_tokens, Some(2));
        assert_eq!(m.stop_reason.as_deref(), Some("completed"));
    }
}
