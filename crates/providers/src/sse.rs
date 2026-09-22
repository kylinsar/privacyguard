//! 流式响应中「承载模型输出文本」的字段位置，以及流片段结束的信号。
//! 供代理在跨事件缓存占位符前缀时使用。

use crate::classify::ApiKind;
use serde_json::Value;

/// 一个文本载体：`key` 标识同一条连续文本流（如 Anthropic 的 content block index），
/// `pointer` 为该事件 JSON 内文本字段的 JSON Pointer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carrier {
    pub key: String,
    pub pointer: String,
}

/// 事件对文本流的终止语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    None,
    /// 以该前缀开头的所有 key 结束
    Prefix(String),
    /// 全部结束
    All,
}

pub fn carriers(api: ApiKind, ev: &Value) -> Vec<Carrier> {
    let mut out = Vec::new();
    match api {
        ApiKind::AnthropicMessages => {
            if ev.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                match ev.pointer("/delta/type").and_then(|t| t.as_str()) {
                    Some("text_delta") if ev.pointer("/delta/text").is_some_and(|v| v.is_string()) => {
                        out.push(Carrier {
                            key: format!("b{idx}"),
                            pointer: "/delta/text".into(),
                        })
                    }
                    Some("input_json_delta")
                        if ev.pointer("/delta/partial_json").is_some_and(|v| v.is_string()) =>
                    {
                        out.push(Carrier {
                            key: format!("b{idx}"),
                            pointer: "/delta/partial_json".into(),
                        })
                    }
                    _ => {}
                }
            }
        }
        ApiKind::ChatCompletions => {
            if let Some(choices) = ev.get("choices").and_then(|c| c.as_array()) {
                for (i, ch) in choices.iter().enumerate() {
                    let cidx = ch.get("index").and_then(|x| x.as_u64()).unwrap_or(i as u64);
                    if ch.pointer("/delta/content").is_some_and(|v| v.is_string()) {
                        out.push(Carrier {
                            key: format!("c{cidx}"),
                            pointer: format!("/choices/{i}/delta/content"),
                        });
                    }
                    if let Some(tcs) = ch.pointer("/delta/tool_calls").and_then(|t| t.as_array()) {
                        for (j, tc) in tcs.iter().enumerate() {
                            let tidx = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(j as u64);
                            if tc.pointer("/function/arguments").is_some_and(|v| v.is_string()) {
                                out.push(Carrier {
                                    key: format!("c{cidx}t{tidx}"),
                                    pointer: format!("/choices/{i}/delta/tool_calls/{j}/function/arguments"),
                                });
                            }
                        }
                    }
                }
            }
        }
        ApiKind::Responses => {
            let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty.ends_with(".delta") && ev.get("delta").is_some_and(|d| d.is_string()) {
                let o = ev.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0);
                let c = ev.get("content_index").and_then(|x| x.as_u64()).unwrap_or(0);
                let kind = ty.trim_end_matches(".delta").rsplit('.').next().unwrap_or("x");
                out.push(Carrier {
                    key: format!("o{o}c{c}{kind}"),
                    pointer: "/delta".into(),
                });
            }
        }
        _ => {}
    }
    out
}

pub fn terminator(api: ApiKind, ev: &Value) -> Terminator {
    let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match api {
        ApiKind::AnthropicMessages => match ty {
            "content_block_stop" => {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                Terminator::Prefix(format!("b{idx}"))
            }
            "message_stop" | "message_delta" | "error" => Terminator::All,
            _ => Terminator::None,
        },
        ApiKind::ChatCompletions => {
            if let Some(choices) = ev.get("choices").and_then(|c| c.as_array()) {
                for (i, ch) in choices.iter().enumerate() {
                    if ch.get("finish_reason").is_some_and(|f| !f.is_null()) {
                        let cidx = ch.get("index").and_then(|x| x.as_u64()).unwrap_or(i as u64);
                        return Terminator::Prefix(format!("c{cidx}"));
                    }
                }
            }
            if ev.get("usage").is_some_and(|u| !u.is_null()) && ev.get("choices").is_some_and(|c| c.as_array().is_some_and(|a| a.is_empty())) {
                return Terminator::All;
            }
            Terminator::None
        }
        ApiKind::Responses => {
            if ty.ends_with(".done") {
                let o = ev.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0);
                return Terminator::Prefix(format!("o{o}"));
            }
            match ty {
                "response.completed" | "response.incomplete" | "response.failed" | "error" => Terminator::All,
                "response.output_item.done" => {
                    let o = ev.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0);
                    Terminator::Prefix(format!("o{o}"))
                }
                _ => Terminator::None,
            }
        }
        _ => Terminator::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_carrier_and_stop() {
        let ev = json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"hi"}});
        assert_eq!(
            carriers(ApiKind::AnthropicMessages, &ev),
            vec![Carrier { key: "b2".into(), pointer: "/delta/text".into() }]
        );
        let stop = json!({"type":"content_block_stop","index":2});
        assert_eq!(terminator(ApiKind::AnthropicMessages, &stop), Terminator::Prefix("b2".into()));
    }

    #[test]
    fn chat_tool_call_carrier() {
        let ev = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"a\""}}]}}]});
        let c = carriers(ApiKind::ChatCompletions, &ev);
        assert_eq!(c[0].key, "c0t1");
        assert_eq!(c[0].pointer, "/choices/0/delta/tool_calls/0/function/arguments");
    }

    #[test]
    fn responses_carrier() {
        let ev = json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"x"});
        let c = carriers(ApiKind::Responses, &ev);
        assert_eq!(c[0].key, "o1c0output_text");
        let done = json!({"type":"response.output_text.done","output_index":1});
        assert_eq!(terminator(ApiKind::Responses, &done), Terminator::Prefix("o1".into()));
    }
}
