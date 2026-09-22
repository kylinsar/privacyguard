use crate::rule::{Rule, RuleKind, Validator};
use regex::Regex;

/// 一次命中：`[start, end)` 为字节偏移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub entity_type: String,
    pub rule_id: String,
    pub priority: i32,
}

pub trait Detector: Send + Sync {
    fn detect(&self, text: &str) -> Vec<Span>;
}

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("规则 {id} 的正则无效: {source}")]
    InvalidRegex {
        id: String,
        #[source]
        source: regex::Error,
    },
}

struct Compiled {
    rule: Rule,
    re: Regex,
}

/// 正则检测器：按规则依次匹配，随后消解重叠。
pub struct RegexDetector {
    compiled: Vec<Compiled>,
}

impl RegexDetector {
    pub fn new(rules: &[Rule]) -> Result<Self, RuleError> {
        let mut compiled = Vec::new();
        for rule in rules {
            if !rule.enabled || rule.kind != RuleKind::Regex {
                continue;
            }
            let re = Regex::new(&rule.pattern).map_err(|e| RuleError::InvalidRegex {
                id: rule.id.clone(),
                source: e,
            })?;
            compiled.push(Compiled {
                rule: rule.clone(),
                re,
            });
        }
        Ok(Self { compiled })
    }

    /// 校验单条规则是否可编译（UI 保存前使用）。
    pub fn validate(rule: &Rule) -> Result<(), RuleError> {
        Regex::new(&rule.pattern)
            .map(|_| ())
            .map_err(|e| RuleError::InvalidRegex {
                id: rule.id.clone(),
                source: e,
            })
    }
}

impl Detector for RegexDetector {
    fn detect(&self, text: &str) -> Vec<Span> {
        let mut spans = Vec::new();
        for c in &self.compiled {
            for caps in c.re.captures_iter(text) {
                let m = match c.rule.group {
                    Some(g) => match caps.get(g) {
                        Some(m) => m,
                        None => continue,
                    },
                    None => caps.get(0).unwrap(),
                };
                if m.start() == m.end() {
                    continue;
                }
                if let Some(v) = c.rule.validator {
                    if !validate(v, m.as_str()) {
                        continue;
                    }
                }
                spans.push(Span {
                    start: m.start(),
                    end: m.end(),
                    entity_type: c.rule.entity_type.clone(),
                    rule_id: c.rule.id.clone(),
                    priority: c.rule.priority,
                });
            }
        }
        resolve_overlaps(spans)
    }
}

fn validate(v: Validator, s: &str) -> bool {
    match v {
        Validator::Luhn => luhn(s),
    }
}

fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 13 {
        return false;
    }
    let mut sum = 0;
    for (i, d) in digits.iter().rev().enumerate() {
        let mut d = *d;
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// 重叠消解：优先级高者胜；同优先级取更长者；再同则取先出现者。
pub fn resolve_overlaps(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| (b.end - b.start).cmp(&(a.end - a.start)))
            .then_with(|| a.start.cmp(&b.start))
    });
    let mut kept: Vec<Span> = Vec::new();
    for s in spans {
        if kept.iter().all(|k| s.end <= k.start || s.start >= k.end) {
            kept.push(s);
        }
    }
    kept.sort_by_key(|s| s.start);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::builtin_rules;

    fn det() -> RegexDetector {
        let mut rules = builtin_rules();
        for r in &mut rules {
            r.enabled = true;
        }
        RegexDetector::new(&rules).unwrap()
    }

    #[test]
    fn detects_email_and_phone() {
        let spans = det().detect("联系 john@example.com 或 13812345678");
        let types: Vec<_> = spans.iter().map(|s| s.entity_type.as_str()).collect();
        assert_eq!(types, vec!["EMAIL", "PHONE"]);
    }

    #[test]
    fn anthropic_key_wins_over_openai_key() {
        let spans = det().detect("key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].rule_id, "builtin.anthropic_key");
    }

    #[test]
    fn luhn_filters_random_digits() {
        let d = det();
        assert!(d.detect("卡号 4111 1111 1111 1111").iter().any(|s| s.entity_type == "CARD"));
        assert!(!d.detect("编号 1234 5678 9012 3456").iter().any(|s| s.entity_type == "CARD"));
    }

    #[test]
    fn assignment_only_redacts_value() {
        let text = "API_KEY=abcdefgh12345678";
        let spans = det().detect(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(&text[spans[0].start..spans[0].end], "abcdefgh12345678");
    }
}
