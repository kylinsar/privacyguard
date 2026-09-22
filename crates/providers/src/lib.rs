//! 各 LLM API 的协议适配：识别接口、抽取/改写用户内容字段、解析用量。
//!
//! 与 `redact` 解耦：改写通过 `FnMut(&str) -> String` 回调完成，本 crate 不关心脱敏细节。

pub mod classify;
pub mod request;
pub mod sse;
pub mod usage;

pub use classify::{classify, ApiKind, Provider};
pub use request::{redact_request_body, request_meta, RequestMeta};
pub use usage::{parse_usage_json, SseUsageCollector, UsageMeta};
