//! 脱敏引擎：规则检测 -> 占位符替换 -> 响应还原。
//!
//! 设计要点：
//! - `Detector` 是抽象接口，首版实现 `RegexDetector`，后续可接入本地模型。
//! - `PlaceholderMap` 会话级、确定性（HMAC），只驻内存。
//! - `StreamRestorer` 处理 SSE 分片场景下的占位符还原。

pub mod detector;
pub mod identity;
pub mod placeholder;
pub mod rule;
pub mod stream;
pub mod synthetic;

pub use detector::{Detector, RegexDetector, RuleError, Span};
pub use identity::{CompositeDetector, IdentityDetector, IdentityField, IDENTITY_RULE_PREFIX};
pub use placeholder::{json_escape_str, placeholder_regex, PlaceholderMap, Style};
pub use rule::{builtin_rules, Rule, RuleKind, Validator};
pub use stream::{holdback_len, StreamRestorer};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 单次脱敏中某条规则的命中统计（不含原值）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedactionEvent {
    pub rule_id: String,
    pub entity_type: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RedactOutcome {
    pub text: String,
    pub events: Vec<RedactionEvent>,
}

impl RedactOutcome {
    pub fn total(&self) -> usize {
        self.events.iter().map(|e| e.count).sum()
    }
}

/// 组合检测器与映射表的门面。可在多请求间共享（`Arc<Redactor>`）。
pub struct Redactor {
    detector: RwLock<Arc<dyn Detector>>,
    map: Arc<Mutex<PlaceholderMap>>,
}

impl Redactor {
    pub fn new(detector: Arc<dyn Detector>, map_capacity: usize) -> Self {
        Self {
            detector: RwLock::new(detector),
            map: Arc::new(Mutex::new(PlaceholderMap::new(map_capacity))),
        }
    }

    pub fn from_rules(rules: &[Rule], map_capacity: usize) -> Result<Self, RuleError> {
        Ok(Self::new(Arc::new(RegexDetector::new(rules)?), map_capacity))
    }

    /// 热更新规则（UI 修改规则后调用），不影响已有映射。
    pub fn replace_detector(&self, detector: Arc<dyn Detector>) {
        *self.detector.write() = detector;
    }

    pub fn reload_rules(&self, rules: &[Rule]) -> Result<(), RuleError> {
        self.replace_detector(Arc::new(RegexDetector::new(rules)?));
        Ok(())
    }

    /// 一次性配置：正则规则 + 隐身身份 + 替换风格。UI 修改任一项后调用。
    pub fn configure(&self, rules: &[Rule], identity: &[IdentityField], style: Style) -> Result<(), RuleError> {
        let regex = RegexDetector::new(rules)?;
        let ident = IdentityDetector::new(identity);
        let detector: Arc<dyn Detector> = if ident.is_empty() {
            Arc::new(regex)
        } else {
            Arc::new(CompositeDetector::new(vec![Box::new(ident), Box::new(regex)]))
        };
        {
            let mut map = self.map.lock();
            map.set_identity(identity);
            map.set_style(style);
        }
        self.replace_detector(detector);
        Ok(())
    }

    pub fn map(&self) -> Arc<Mutex<PlaceholderMap>> {
        self.map.clone()
    }

    pub fn redact_text(&self, text: &str) -> RedactOutcome {
        let spans = self.detector.read().detect(text);
        if spans.is_empty() {
            return RedactOutcome {
                text: text.to_string(),
                events: Vec::new(),
            };
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut map = self.map.lock();
        for s in spans {
            out.push_str(&text[last..s.start]);
            let value = &text[s.start..s.end];
            out.push_str(&map.placeholder_for(value, &s.entity_type));
            *counts.entry((s.rule_id, s.entity_type)).or_insert(0) += 1;
            last = s.end;
        }
        out.push_str(&text[last..]);
        RedactOutcome {
            text: out,
            events: counts
                .into_iter()
                .map(|((rule_id, entity_type), count)| RedactionEvent {
                    rule_id,
                    entity_type,
                    count,
                })
                .collect(),
        }
    }

    /// 非流式还原（整段文本）。
    pub fn restore_text(&self, text: &str, json_escape: bool) -> (String, usize) {
        self.map.lock().restore_text(text, json_escape)
    }

    pub fn stream_restorer(&self, json_escape: bool) -> StreamRestorer {
        StreamRestorer::new(self.map.clone(), json_escape)
    }

    /// 仅检测不替换（规则测试面板使用）。
    pub fn detect(&self, text: &str) -> Vec<Span> {
        self.detector.read().detect(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_synthetic_roundtrip() {
        let r = Redactor::from_rules(&builtin_rules(), 1000).unwrap();
        let ident = vec![IdentityField {
            id: "me".into(),
            entity_type: "NAME".into(),
            real_values: vec!["王大锤".into()],
            alias: "陈小明".into(),
            ..Default::default()
        }, IdentityField {
            id: "mail".into(),
            entity_type: "EMAIL".into(),
            real_values: vec!["dachui@real.com".into()],
            alias: "xiaoming@fake.io".into(),
            ..Default::default()
        }];
        r.configure(&builtin_rules(), &ident, Style::Synthetic).unwrap();
        let o = r.redact_text("我是王大锤，邮箱 dachui@real.com，同事邮箱 peer@corp.com");
        assert_eq!(o.text.split("，").next().unwrap(), "我是陈小明");
        assert!(o.text.contains("xiaoming@fake.io"));
        assert!(!o.text.contains("peer@corp.com") && !o.text.contains("PG_"), "{}", o.text);
        // 身份字段命中优先于正则邮箱规则
        assert!(o.events.iter().any(|e| e.rule_id == "identity.mail"));
        assert!(o.events.iter().any(|e| e.rule_id == "builtin.email" && e.count == 1));
        let (back, n) = r.restore_text(&o.text, false);
        assert_eq!(n, 3);
        assert_eq!(back, "我是王大锤，邮箱 dachui@real.com，同事邮箱 peer@corp.com");
    }

    #[test]
    fn roundtrip() {
        let r = Redactor::from_rules(&builtin_rules(), 1000).unwrap();
        let o = r.redact_text("发邮件给 bob@x.io，密码 password=SuperSecret123");
        assert!(!o.text.contains("bob@x.io"));
        assert!(!o.text.contains("SuperSecret123"));
        assert_eq!(o.total(), 2);
        let (back, n) = r.restore_text(&o.text, false);
        assert_eq!(n, 2);
        assert_eq!(back, "发邮件给 bob@x.io，密码 password=SuperSecret123");
    }
}
