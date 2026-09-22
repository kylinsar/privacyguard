//! 会话级「原值 <-> 替换值」映射表。
//!
//! 替换值有三种来源：
//! - 占位符 `PG_<ENTITY>_<8hex>`（默认）；
//! - 拟真值（`Style::Synthetic`，同类型的假邮箱 / 假号码等）；
//! - 隐身身份（用户为自己的真实信息指定的固定别名，见 `identity`）。
//!
//! 还原时用 Aho-Corasick 在文本中查找所有已知替换值并换回原值。

use crate::identity::IdentityField;
use crate::synthetic;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;

pub const PLACEHOLDER_PREFIX: &str = "PG_";
const HASH_HEX_LEN: usize = 8;
const SYNTHETIC_ATTEMPTS: usize = 8;

/// 占位符：`PG_<ENTITY>_<8hex>`。实体名限定为 `[A-Z0-9]`，因此在 JSON 字符串内无需转义。
pub fn placeholder_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"PG_[A-Z0-9]+_[0-9a-f]{8}").unwrap())
}

pub fn normalize_entity(entity: &str) -> String {
    let s: String = entity
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if s.is_empty() {
        "PRIVATE".to_string()
    } else {
        s
    }
}

/// 未被隐身身份覆盖的敏感值用什么替换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// `PG_EMAIL_1a2b3c4d`
    #[default]
    Placeholder,
    /// 同类型拟真假值（邮箱 / 手机 / 身份证 / 银行卡 / IP），其余类型仍用占位符
    Synthetic,
}

/// 会话级确定性映射：同一原值总是映射到同一替换值（HMAC），保证重发历史字节一致。
pub struct PlaceholderMap {
    key: [u8; 32],
    style: Style,
    /// 动态映射（检测到的值）
    forward: HashMap<String, String>,
    reverse: HashMap<String, String>,
    order: VecDeque<String>,
    cap: usize,
    /// 隐身身份的固定映射（不参与淘汰）
    fixed_forward: HashMap<String, String>,
    /// 忽略大小写字段：小写真实值 -> 别名
    fixed_forward_ci: HashMap<String, String>,
    fixed_reverse: HashMap<String, String>,
    /// 非占位符形式的替换值集合（用于流式 holdback 前缀判断）
    alias_set: BTreeSet<Vec<u8>>,
    max_placeholder_len: usize,
    max_alias_len: usize,
    ac: Option<AhoCorasick>,
    ac_dirty: bool,
}

impl PlaceholderMap {
    pub fn new(cap: usize) -> Self {
        let mut key = [0u8; 32];
        rand::fill(&mut key);
        Self::with_key(key, cap)
    }

    pub fn with_key(key: [u8; 32], cap: usize) -> Self {
        Self {
            key,
            style: Style::Placeholder,
            forward: HashMap::new(),
            reverse: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(16),
            fixed_forward: HashMap::new(),
            fixed_forward_ci: HashMap::new(),
            fixed_reverse: HashMap::new(),
            alias_set: BTreeSet::new(),
            max_placeholder_len: 0,
            max_alias_len: 0,
            ac: None,
            ac_dirty: false,
        }
    }

    pub fn style(&self) -> Style {
        self.style
    }

    /// 切换替换风格。已有映射保留，只影响之后新出现的值。
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    /// 载入隐身身份映射，替换之前的身份配置。
    pub fn set_identity(&mut self, fields: &[IdentityField]) {
        for alias in self.fixed_reverse.keys() {
            if !is_placeholder(alias) {
                self.alias_set.remove(alias.as_bytes());
            }
        }
        self.fixed_forward.clear();
        self.fixed_forward_ci.clear();
        self.fixed_reverse.clear();
        for f in fields.iter().filter(|f| f.is_usable()) {
            let alias = f.alias.trim().to_string();
            let reals = f.effective_reals();
            let Some(primary) = reals.first() else { continue };
            // 若别名已作为动态替换值存在且指向别的原值，让固定映射优先（fixed_reverse 先查）
            self.fixed_reverse.entry(alias.clone()).or_insert_with(|| primary.to_string());
            for r in &reals {
                self.fixed_forward.insert(r.to_string(), alias.clone());
                if f.case_insensitive {
                    self.fixed_forward_ci.insert(r.to_lowercase(), alias.clone());
                }
            }
            self.note_alias(&alias);
        }
        self.ac_dirty = true;
    }

    pub fn len(&self) -> usize {
        self.reverse.len() + self.fixed_reverse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty() && self.fixed_reverse.is_empty()
    }

    /// 已知占位符的最大长度（用于流式还原时决定保留多少尾部字节）。
    pub fn max_placeholder_len(&self) -> usize {
        self.max_placeholder_len
    }

    /// 任一替换值的最大长度。
    pub fn max_len(&self) -> usize {
        self.max_placeholder_len.max(self.max_alias_len)
    }

    fn note_alias(&mut self, alias: &str) {
        if is_placeholder(alias) {
            self.max_placeholder_len = self.max_placeholder_len.max(alias.len());
        } else {
            self.max_alias_len = self.max_alias_len.max(alias.len());
            self.alias_set.insert(alias.as_bytes().to_vec());
        }
    }

    fn lookup_fixed(&self, value: &str) -> Option<&String> {
        self.fixed_forward
            .get(value)
            .or_else(|| self.fixed_forward_ci.get(&value.to_lowercase()))
    }

    /// 某个替换值是否已被占用（指向别的原值）。
    fn alias_taken(&self, alias: &str, value: &str) -> bool {
        self.fixed_reverse.get(alias).is_some_and(|v| v != value)
            || self.reverse.get(alias).is_some_and(|v| v != value)
            || self.fixed_forward.contains_key(alias)
            || self.forward.contains_key(alias)
    }

    pub fn placeholder_for(&mut self, value: &str, entity: &str) -> String {
        if let Some(a) = self.lookup_fixed(value) {
            return a.clone();
        }
        if let Some(p) = self.forward.get(value) {
            return p.clone();
        }
        let entity = normalize_entity(entity);
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac key");
        mac.update(entity.as_bytes());
        mac.update(b"\0");
        mac.update(value.as_bytes());
        let digest = mac.finalize().into_bytes();

        if self.style == Style::Synthetic && synthetic::supports(&entity) {
            for attempt in 0..SYNTHETIC_ATTEMPTS {
                if let Some(fake) = synthetic::generate(&entity, value, &digest, attempt) {
                    if fake != value && !self.alias_taken(&fake, value) {
                        self.insert(value.to_string(), fake.clone());
                        return fake;
                    }
                }
            }
        }

        let full = hex::encode(digest);
        let mut placeholder = format!("{PLACEHOLDER_PREFIX}{entity}_{}", &full[..HASH_HEX_LEN]);
        // 极低概率的哈希碰撞：换用后续 hex 位
        let mut offset = 1;
        while self.alias_taken(&placeholder, value) && offset + HASH_HEX_LEN <= full.len() {
            placeholder = format!("{PLACEHOLDER_PREFIX}{entity}_{}", &full[offset..offset + HASH_HEX_LEN]);
            offset += 1;
        }
        self.insert(value.to_string(), placeholder.clone());
        placeholder
    }

    fn insert(&mut self, value: String, alias: String) {
        self.note_alias(&alias);
        self.forward.insert(value.clone(), alias.clone());
        self.reverse.insert(alias.clone(), value);
        self.order.push_back(alias);
        self.ac_dirty = true;
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                if let Some(v) = self.reverse.remove(&old) {
                    self.forward.remove(&v);
                }
                if !is_placeholder(&old) && !self.fixed_reverse.contains_key(&old) {
                    self.alias_set.remove(old.as_bytes());
                }
            }
        }
    }

    pub fn original_for(&self, alias: &str) -> Option<&str> {
        self.fixed_reverse
            .get(alias)
            .or_else(|| self.reverse.get(alias))
            .map(|s| s.as_str())
    }

    fn ensure_ac(&mut self) {
        if !self.ac_dirty {
            return;
        }
        self.ac_dirty = false;
        let pats: Vec<&[u8]> = self
            .fixed_reverse
            .keys()
            .chain(self.reverse.keys())
            .map(|s| s.as_bytes())
            .collect();
        self.ac = if pats.is_empty() {
            None
        } else {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&pats)
                .ok()
        };
    }

    /// 在任意文本中还原替换值。`json_escape` 为 true 时把原值按 JSON 字符串内容转义
    /// （用于直接在 JSON / SSE 原文上做文本级替换）。返回 (新文本, 还原次数)。
    pub fn restore_text(&mut self, text: &str, json_escape: bool) -> (String, usize) {
        self.ensure_ac();
        let Some(ac) = &self.ac else { return (text.to_string(), 0) };
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let mut count = 0;
        for m in ac.find_iter(text) {
            if !text.is_char_boundary(m.start()) || !text.is_char_boundary(m.end()) {
                continue;
            }
            let alias = &text[m.start()..m.end()];
            let Some(v) = self.original_for(alias) else { continue };
            out.push_str(&text[last..m.start()]);
            count += 1;
            if json_escape {
                out.push_str(&json_escape_str(v));
            } else {
                out.push_str(v);
            }
            last = m.end();
        }
        out.push_str(&text[last..]);
        (out, count)
    }

    /// 流式还原时需要保留的尾部字节数：尾部可能是某个占位符或别名的前缀。
    /// 返回的切分点总在 UTF-8 字符边界上。
    pub fn holdback(&self, buf: &[u8]) -> usize {
        let ph = crate::stream::holdback_len(buf, self.max_placeholder_len);
        let al = self.alias_holdback(buf);
        ph.max(al)
    }

    fn alias_holdback(&self, buf: &[u8]) -> usize {
        if self.max_alias_len == 0 || buf.is_empty() {
            return 0;
        }
        // 窗口最多 max_alias_len - 1 字节（完整别名会被 restore_text 直接处理，除非它还是更长别名的前缀）
        let window_start = buf.len().saturating_sub(self.max_alias_len);
        for i in window_start..buf.len() {
            let suffix = &buf[i..];
            // 只有字符首字节才可能是别名开头
            if (suffix[0] & 0xC0) == 0x80 {
                continue;
            }
            if self.is_alias_prefix(suffix) {
                return buf.len() - i;
            }
        }
        0
    }

    /// `s` 是否是某个别名的严格前缀。
    fn is_alias_prefix(&self, s: &[u8]) -> bool {
        self.alias_set
            .range(s.to_vec()..)
            .take_while(|a| a.starts_with(s))
            .any(|a| a.len() > s.len())
    }

    pub fn clear(&mut self) {
        self.forward.clear();
        self.reverse.clear();
        self.order.clear();
        self.alias_set.retain(|a| {
            self.fixed_reverse
                .contains_key(std::str::from_utf8(a).unwrap_or_default())
        });
        self.ac_dirty = true;
    }
}

fn is_placeholder(s: &str) -> bool {
    placeholder_regex().is_match(s) && s.starts_with(PLACEHOLDER_PREFIX)
}

/// 把字符串转成 JSON 字符串字面量的内部内容（不含两侧引号）。
pub fn json_escape_str(s: &str) -> String {
    let quoted = serde_json::to_string(s).expect("string is always serializable");
    quoted[1..quoted.len() - 1].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_within_session() {
        let mut m = PlaceholderMap::with_key([7u8; 32], 100);
        let a = m.placeholder_for("john@example.com", "EMAIL");
        let b = m.placeholder_for("john@example.com", "EMAIL");
        assert_eq!(a, b);
        assert!(a.starts_with("PG_EMAIL_"));
        assert_eq!(a.len(), "PG_EMAIL_".len() + 8);
    }

    #[test]
    fn restore_with_json_escape() {
        let mut m = PlaceholderMap::with_key([1u8; 32], 100);
        let p = m.placeholder_for("pa\"ss\\word", "SECRET");
        let (out, n) = m.restore_text(&format!("{{\"x\":\"{p}\"}}"), true);
        assert_eq!(n, 1);
        assert_eq!(out, r#"{"x":"pa\"ss\\word"}"#);
    }

    #[test]
    fn unknown_placeholder_untouched() {
        let mut m = PlaceholderMap::with_key([1u8; 32], 100);
        m.placeholder_for("x", "EMAIL");
        let (out, n) = m.restore_text("PG_EMAIL_deadbeef stays", false);
        assert_eq!(n, 0);
        assert_eq!(out, "PG_EMAIL_deadbeef stays");
    }

    #[test]
    fn synthetic_style_generates_realistic_values() {
        let mut m = PlaceholderMap::with_key([2u8; 32], 100);
        m.set_style(Style::Synthetic);
        let mail = m.placeholder_for("john@example.com", "EMAIL");
        assert!(mail.contains('@') && !mail.starts_with("PG_"));
        let phone = m.placeholder_for("13800138000", "PHONE");
        assert!(phone.starts_with("138") && phone.len() == 11 && phone != "13800138000");
        // 不支持拟真的类型仍是占位符
        let sec = m.placeholder_for("sk-ant-xxx", "SECRET");
        assert!(sec.starts_with("PG_SECRET_"));
        let (back, n) = m.restore_text(&format!("call {phone} or {mail} / {sec}"), false);
        assert_eq!(n, 3);
        assert_eq!(back, "call 13800138000 or john@example.com / sk-ant-xxx");
    }

    fn identity() -> Vec<IdentityField> {
        vec![
            IdentityField {
                id: "name".into(),
                entity_type: "NAME".into(),
                real_values: vec!["张三".into(), "Zhang San".into()],
                alias: "李四".into(),
                case_insensitive: true,
                ..Default::default()
            },
            IdentityField {
                id: "org".into(),
                entity_type: "ORG".into(),
                real_values: vec!["真实公司".into()],
                alias: "李四科技".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn identity_alias_roundtrip() {
        let mut m = PlaceholderMap::with_key([5u8; 32], 100);
        m.set_identity(&identity());
        assert_eq!(m.placeholder_for("张三", "NAME"), "李四");
        assert_eq!(m.placeholder_for("zhang san", "NAME"), "李四");
        assert_eq!(m.placeholder_for("真实公司", "ORG"), "李四科技");
        // 还原：最长匹配优先，"李四科技" 不会被拆成 "李四" + "科技"
        let (back, n) = m.restore_text("李四在李四科技上班", false);
        assert_eq!(n, 2);
        assert_eq!(back, "张三在真实公司上班");
    }

    #[test]
    fn holdback_covers_alias_prefixes() {
        let mut m = PlaceholderMap::with_key([5u8; 32], 100);
        m.set_identity(&identity());
        let li = "李".as_bytes();
        // 尾部是 "李"（"李四" 的前缀）需要保留
        let mut buf = b"hello ".to_vec();
        buf.extend_from_slice(li);
        assert_eq!(m.holdback(&buf), li.len());
        // 尾部是完整的 "李四" 但也是 "李四科技" 的前缀，仍需保留
        let mut buf2 = b"x ".to_vec();
        buf2.extend_from_slice("李四".as_bytes());
        assert_eq!(m.holdback(&buf2), "李四".len());
        // 普通文本不保留
        assert_eq!(m.holdback(b"plain text"), 0);
        // 占位符前缀仍然生效
        m.placeholder_for("a@b.c", "EMAIL");
        assert_eq!(m.holdback(b"see PG_EM"), 5);
    }
}
