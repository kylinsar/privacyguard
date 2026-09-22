//! 隐身身份：用户自定义的「真实值 -> 隐身值」映射。
//!
//! 与正则规则不同，姓名、公司、地址这类信息无法靠模式识别，但用户清楚自己的真实值是什么。
//! 这里按字面精确匹配真实值（可忽略大小写），命中后替换为用户指定的隐身值（而不是 `PG_xxx` 占位符），
//! 让模型看到的是一套自洽的假身份；响应回来时再把隐身值换回真实值。

use crate::detector::{resolve_overlaps, Detector, Span};
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};

/// 身份映射的优先级：高于所有正则规则。
pub const IDENTITY_PRIORITY: i32 = 1000;
pub const IDENTITY_RULE_PREFIX: &str = "identity.";

/// 一条身份字段：若干真实写法 -> 一个隐身值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IdentityField {
    pub id: String,
    /// 显示名，如「姓名」「手机号」「公司」
    pub label: String,
    /// 实体类型（NAME / PHONE / EMAIL / ORG / ADDRESS / ID_CARD / CUSTOM …）
    pub entity_type: String,
    /// 真实值的各种写法（如 "张三"、"Zhang San"、"zhangsan"）；第一个视为主写法，还原时使用
    pub real_values: Vec<String>,
    /// 隐身值
    pub alias: String,
    /// 匹配时忽略大小写（对 ASCII 有效）
    pub case_insensitive: bool,
    pub enabled: bool,
}

impl Default for IdentityField {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            entity_type: "CUSTOM".into(),
            real_values: Vec::new(),
            alias: String::new(),
            case_insensitive: false,
            enabled: true,
        }
    }
}

impl IdentityField {
    /// 过滤掉空白、与隐身值相同的真实值，返回有效写法。
    pub fn effective_reals(&self) -> Vec<&str> {
        let alias = self.alias.trim();
        self.real_values
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != alias)
            .collect()
    }

    pub fn is_usable(&self) -> bool {
        self.enabled && !self.alias.trim().is_empty() && !self.effective_reals().is_empty()
    }
}

struct Pattern {
    field_idx: usize,
}

/// 身份字面匹配检测器。
pub struct IdentityDetector {
    fields: Vec<IdentityField>,
    patterns: Vec<Pattern>,
    ac: Option<AhoCorasick>,
}

impl IdentityDetector {
    pub fn new(fields: &[IdentityField]) -> Self {
        let fields: Vec<IdentityField> = fields.iter().filter(|f| f.is_usable()).cloned().collect();
        let mut pats: Vec<Vec<u8>> = Vec::new();
        let mut patterns = Vec::new();
        // 只要有任一字段要求忽略大小写，就整体用 ASCII 大小写不敏感匹配，
        // 对大小写敏感字段在命中后再做精确校验
        let any_ci = fields.iter().any(|f| f.case_insensitive);
        for (i, f) in fields.iter().enumerate() {
            for r in f.effective_reals() {
                pats.push(r.as_bytes().to_vec());
                patterns.push(Pattern { field_idx: i });
            }
        }
        let ac = if pats.is_empty() {
            None
        } else {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .ascii_case_insensitive(any_ci)
                .build(&pats)
                .ok()
        };
        Self { fields, patterns, ac }
    }

    pub fn is_empty(&self) -> bool {
        self.ac.is_none()
    }

    pub fn fields(&self) -> &[IdentityField] {
        &self.fields
    }
}

impl Detector for IdentityDetector {
    fn detect(&self, text: &str) -> Vec<Span> {
        let Some(ac) = &self.ac else { return Vec::new() };
        let mut spans = Vec::new();
        for m in ac.find_iter(text) {
            let p = &self.patterns[m.pattern().as_usize()];
            let f = &self.fields[p.field_idx];
            let hit = &text[m.start()..m.end()];
            if !f.case_insensitive && !f.effective_reals().iter().any(|r| *r == hit) {
                continue;
            }
            // 避免命中多字节字符中间（AC 按字节匹配，但模式本身是合法 UTF-8，
            // 只需确认边界落在字符边界上）
            if !text.is_char_boundary(m.start()) || !text.is_char_boundary(m.end()) {
                continue;
            }
            spans.push(Span {
                start: m.start(),
                end: m.end(),
                entity_type: f.entity_type.clone(),
                rule_id: format!("{IDENTITY_RULE_PREFIX}{}", f.id),
                priority: IDENTITY_PRIORITY,
            });
        }
        spans
    }
}

/// 组合多个检测器，统一消解重叠。
pub struct CompositeDetector {
    parts: Vec<Box<dyn Detector>>,
}

impl CompositeDetector {
    pub fn new(parts: Vec<Box<dyn Detector>>) -> Self {
        Self { parts }
    }
}

impl Detector for CompositeDetector {
    fn detect(&self, text: &str) -> Vec<Span> {
        let mut all = Vec::new();
        for p in &self.parts {
            all.extend(p.detect(text));
        }
        resolve_overlaps(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_field() -> IdentityField {
        IdentityField {
            id: "name".into(),
            label: "姓名".into(),
            entity_type: "NAME".into(),
            real_values: vec!["张三".into(), "Zhang San".into()],
            alias: "李四".into(),
            case_insensitive: true,
            ..Default::default()
        }
    }

    #[test]
    fn matches_all_variants() {
        let d = IdentityDetector::new(&[name_field()]);
        let text = "我是张三，英文名 zhang san。";
        let spans = d.detect(text);
        assert_eq!(spans.len(), 2);
        assert_eq!(&text[spans[0].start..spans[0].end], "张三");
        assert_eq!(&text[spans[1].start..spans[1].end], "zhang san");
        assert_eq!(spans[0].rule_id, "identity.name");
        assert_eq!(spans[0].priority, IDENTITY_PRIORITY);
    }

    #[test]
    fn case_sensitive_field_requires_exact() {
        let mut f = name_field();
        f.case_insensitive = false;
        // 另一个字段要求忽略大小写，会让 AC 整体大小写不敏感，但本字段仍需精确
        let other = IdentityField {
            id: "mail".into(),
            entity_type: "EMAIL".into(),
            real_values: vec!["Me@Corp.com".into()],
            alias: "x@y.z".into(),
            case_insensitive: true,
            ..Default::default()
        };
        let d = IdentityDetector::new(&[f, other]);
        let spans = d.detect("zhang san me@corp.com Zhang San");
        let hits: Vec<_> = spans.iter().map(|s| s.rule_id.as_str()).collect();
        assert_eq!(hits, vec!["identity.mail", "identity.name"]);
    }

    #[test]
    fn unusable_fields_ignored() {
        let mut f = name_field();
        f.alias = "".into();
        assert!(IdentityDetector::new(&[f]).is_empty());
    }
}
