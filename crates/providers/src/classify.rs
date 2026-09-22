use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    OpenAi,
    Anthropic,
    /// ChatGPT 订阅登录下 Codex 使用的后端
    ChatGptCodex,
    #[default]
    Unknown,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::Anthropic => "anthropic",
            Provider::ChatGptCodex => "chatgpt_codex",
            Provider::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    ChatCompletions,
    Responses,
    AnthropicMessages,
    /// 已识别的提供商但非对话接口（模型列表、计数 token 等），透传不改
    Passthrough,
    #[default]
    Unknown,
}

impl ApiKind {
    pub fn is_chat(&self) -> bool {
        matches!(
            self,
            ApiKind::ChatCompletions | ApiKind::Responses | ApiKind::AnthropicMessages
        )
    }
}

/// 拦截的上游域名（用于 CONNECT 决策与 PAC 生成）。
pub const INTERCEPT_HOSTS: &[&str] = &["api.openai.com", "api.anthropic.com", "chatgpt.com"];

pub fn host_provider(host: &str) -> Provider {
    let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    if h == "api.openai.com" {
        Provider::OpenAi
    } else if h == "api.anthropic.com" {
        Provider::Anthropic
    } else if h == "chatgpt.com" || h.ends_with(".chatgpt.com") {
        Provider::ChatGptCodex
    } else {
        Provider::Unknown
    }
}

/// 按 host + path 判定协议。path 需以 `/` 开头且不含 query。
pub fn classify(host: &str, path: &str) -> (Provider, ApiKind) {
    let provider = host_provider(host);
    let path = path.split('?').next().unwrap_or(path);
    let kind = match provider {
        Provider::OpenAi => {
            if path.ends_with("/chat/completions") {
                ApiKind::ChatCompletions
            } else if path.ends_with("/responses") {
                ApiKind::Responses
            } else {
                ApiKind::Passthrough
            }
        }
        Provider::ChatGptCodex => {
            if path.ends_with("/responses") {
                ApiKind::Responses
            } else {
                ApiKind::Passthrough
            }
        }
        Provider::Anthropic => {
            if path.ends_with("/messages") {
                ApiKind::AnthropicMessages
            } else {
                ApiKind::Passthrough
            }
        }
        Provider::Unknown => classify_by_path(path),
    };
    (provider, kind)
}

/// 反向代理入口下 host 是 127.0.0.1，只能靠路径判定。
pub fn classify_by_path(path: &str) -> ApiKind {
    let path = path.split('?').next().unwrap_or(path);
    if path.ends_with("/chat/completions") {
        ApiKind::ChatCompletions
    } else if path.ends_with("/responses") {
        ApiKind::Responses
    } else if path.ends_with("/messages") {
        ApiKind::AnthropicMessages
    } else {
        ApiKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known() {
        assert_eq!(
            classify("api.anthropic.com", "/v1/messages?beta=true"),
            (Provider::Anthropic, ApiKind::AnthropicMessages)
        );
        assert_eq!(
            classify("api.openai.com", "/v1/responses"),
            (Provider::OpenAi, ApiKind::Responses)
        );
        assert_eq!(
            classify("chatgpt.com", "/backend-api/codex/responses"),
            (Provider::ChatGptCodex, ApiKind::Responses)
        );
        assert_eq!(
            classify("api.openai.com", "/v1/models"),
            (Provider::OpenAi, ApiKind::Passthrough)
        );
    }
}
