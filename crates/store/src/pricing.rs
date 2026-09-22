//! 模型定价（USD / 百万 token）与费用估算。
//!
//! 默认表只是参考价，用户可在「使用监控」页修改；ChatGPT / Claude 订阅登录的流量
//! 实际上不按 token 计费，这里算出的只是「等效 API 费用」。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPrice {
    /// 模型名前缀（不区分大小写），最长前缀优先
    pub model_prefix: String,
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_per_m: f64,
}

impl Default for ModelPrice {
    fn default() -> Self {
        Self {
            model_prefix: String::new(),
            input_per_m: 0.0,
            output_per_m: 0.0,
            cache_read_per_m: 0.0,
            cache_write_per_m: 0.0,
        }
    }
}

fn p(prefix: &str, i: f64, o: f64, cr: f64, cw: f64) -> ModelPrice {
    ModelPrice {
        model_prefix: prefix.into(),
        input_per_m: i,
        output_per_m: o,
        cache_read_per_m: cr,
        cache_write_per_m: cw,
    }
}

/// 参考价格表（USD / 1M tokens）。
pub fn default_prices() -> Vec<ModelPrice> {
    vec![
        // Anthropic
        p("claude-opus-4", 15.0, 75.0, 1.5, 18.75),
        p("claude-sonnet-4", 3.0, 15.0, 0.3, 3.75),
        p("claude-3-7-sonnet", 3.0, 15.0, 0.3, 3.75),
        p("claude-3-5-sonnet", 3.0, 15.0, 0.3, 3.75),
        p("claude-haiku-4", 1.0, 5.0, 0.1, 1.25),
        p("claude-3-5-haiku", 0.8, 4.0, 0.08, 1.0),
        p("claude", 3.0, 15.0, 0.3, 3.75),
        // OpenAI
        p("gpt-5-codex", 1.25, 10.0, 0.125, 0.0),
        p("gpt-5-nano", 0.05, 0.4, 0.005, 0.0),
        p("gpt-5-mini", 0.25, 2.0, 0.025, 0.0),
        p("gpt-5", 1.25, 10.0, 0.125, 0.0),
        p("gpt-4.1-nano", 0.1, 0.4, 0.025, 0.0),
        p("gpt-4.1-mini", 0.4, 1.6, 0.1, 0.0),
        p("gpt-4.1", 2.0, 8.0, 0.5, 0.0),
        p("gpt-4o-mini", 0.15, 0.6, 0.075, 0.0),
        p("gpt-4o", 2.5, 10.0, 1.25, 0.0),
        p("codex-mini", 1.5, 6.0, 0.375, 0.0),
        p("o4-mini", 1.1, 4.4, 0.275, 0.0),
        p("o3-pro", 20.0, 80.0, 0.0, 0.0),
        p("o3", 2.0, 8.0, 0.5, 0.0),
    ]
}

/// 最长前缀匹配。
pub fn price_for<'a>(prices: &'a [ModelPrice], model: &str) -> Option<&'a ModelPrice> {
    let m = model.to_ascii_lowercase();
    prices
        .iter()
        .filter(|p| !p.model_prefix.is_empty() && m.starts_with(&p.model_prefix.to_ascii_lowercase()))
        .max_by_key(|p| p.model_prefix.len())
}

/// 估算一次调用的费用（USD）。
///
/// OpenAI 的 `input_tokens` 包含缓存命中部分，Anthropic 的 `input_tokens` 不含缓存部分，
/// 因此按 provider 区分计算。
pub fn estimate_cost(
    prices: &[ModelPrice],
    provider: &str,
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> Option<f64> {
    let pr = price_for(prices, model)?;
    let uncached_input = if provider == "openai" {
        input.saturating_sub(cache_read)
    } else {
        input
    };
    let m = 1_000_000.0;
    Some(
        uncached_input as f64 / m * pr.input_per_m
            + output as f64 / m * pr.output_per_m
            + cache_read as f64 / m * pr.cache_read_per_m
            + cache_write as f64 / m * pr.cache_write_per_m,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let prices = default_prices();
        assert_eq!(price_for(&prices, "gpt-5-codex").unwrap().model_prefix, "gpt-5-codex");
        assert_eq!(price_for(&prices, "GPT-5-2025-08-07").unwrap().model_prefix, "gpt-5");
        assert_eq!(price_for(&prices, "claude-sonnet-4-5-20250929").unwrap().model_prefix, "claude-sonnet-4");
        assert!(price_for(&prices, "llama").is_none());
    }

    #[test]
    fn cost_math() {
        let prices = vec![p("m", 1.0, 2.0, 0.1, 0.5)];
        // openai: input 含 cached
        let c = estimate_cost(&prices, "openai", "m", 1_000_000, 500_000, 400_000, 0).unwrap();
        assert!((c - (0.6 + 1.0 + 0.04)).abs() < 1e-9);
        // anthropic: input 不含 cached
        let c = estimate_cost(&prices, "anthropic", "m", 1_000_000, 0, 1_000_000, 1_000_000).unwrap();
        assert!((c - (1.0 + 0.1 + 0.5)).abs() < 1e-9);
    }
}
