use crate::placeholder::{PlaceholderMap, PLACEHOLDER_PREFIX};
use parking_lot::Mutex;
use std::sync::Arc;

/// 流式（SSE）还原器：占位符可能被拆到两个 chunk 中，因此始终保留可能是占位符前缀的尾部字节。
pub struct StreamRestorer {
    map: Arc<Mutex<PlaceholderMap>>,
    carry: Vec<u8>,
    json_escape: bool,
    restored: usize,
}

impl StreamRestorer {
    pub fn new(map: Arc<Mutex<PlaceholderMap>>, json_escape: bool) -> Self {
        Self {
            map,
            carry: Vec::new(),
            json_escape,
            restored: 0,
        }
    }

    pub fn restored_count(&self) -> usize {
        self.restored
    }

    /// 喂入一个 chunk，返回可以安全下发的字节。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if chunk.is_empty() {
            return Vec::new();
        }
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(chunk);

        let mut map = self.map.lock();
        if map.is_empty() {
            drop(map);
            return buf;
        }
        let hold = map.holdback(&buf);
        let emit_end = buf.len() - hold;
        let (head, tail) = buf.split_at(emit_end);
        let out = restore_bytes(&mut map, head, self.json_escape, &mut self.restored);
        self.carry = tail.to_vec();
        out
    }

    /// 流结束：把剩余尾部全部处理并返回。
    pub fn finish(&mut self) -> Vec<u8> {
        let buf = std::mem::take(&mut self.carry);
        if buf.is_empty() {
            return buf;
        }
        let mut map = self.map.lock();
        restore_bytes(&mut map, &buf, self.json_escape, &mut self.restored)
    }
}

fn restore_bytes(map: &mut PlaceholderMap, bytes: &[u8], json_escape: bool, counter: &mut usize) -> Vec<u8> {
    // 占位符全为 ASCII；对无效 UTF-8 的分片用 lossy 会破坏多字节字符，
    // 因此这里只在合法 UTF-8 边界内做替换，非法尾部原样返回。
    match std::str::from_utf8(bytes) {
        Ok(s) => {
            let (out, n) = map.restore_text(s, json_escape);
            *counter += n;
            out.into_bytes()
        }
        Err(e) => {
            let valid = e.valid_up_to();
            let (s, rest) = bytes.split_at(valid);
            let s = std::str::from_utf8(s).unwrap();
            let (out, n) = map.restore_text(s, json_escape);
            *counter += n;
            let mut v = out.into_bytes();
            v.extend_from_slice(rest);
            v
        }
    }
}

/// 计算需要保留的尾部长度：从尾部最多 `max_len` 字节中找最靠前的、可能是占位符前缀的位置。
/// 返回值总是落在 ASCII 字符 `P` 上，因此对 UTF-8 字符串按此长度切分是安全的。
/// 只覆盖 `PG_` 占位符；含别名 / 拟真值时请用 `PlaceholderMap::holdback`。
pub fn holdback_len(buf: &[u8], max_len: usize) -> usize {
    if max_len == 0 {
        return 0;
    }
    let window_start = buf.len().saturating_sub(max_len);
    let window = &buf[window_start..];
    // 从窗口内每个 'P' 出发，看后缀是否是占位符的合法前缀
    for (i, b) in window.iter().enumerate() {
        if *b == b'P' && is_placeholder_prefix(&window[i..]) {
            return window.len() - i;
        }
    }
    0
}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// 判断字节串是否是 `PG_[A-Z0-9]+_[0-9a-f]{8}` 的严格前缀（不含完整匹配）。
fn is_placeholder_prefix(s: &[u8]) -> bool {
    let prefix = PLACEHOLDER_PREFIX.as_bytes();
    let n = s.len().min(prefix.len());
    if s[..n] != prefix[..n] {
        return false;
    }
    if s.len() <= prefix.len() {
        return true;
    }
    let rest = &s[prefix.len()..];
    // 实体段
    let mut i = 0;
    while i < rest.len() && (rest[i].is_ascii_uppercase() || rest[i].is_ascii_digit()) {
        i += 1;
    }
    if i == 0 {
        return false;
    }
    if i == rest.len() {
        return true;
    }
    if rest[i] != b'_' {
        return false;
    }
    i += 1;
    let hex_start = i;
    while i < rest.len() && is_lower_hex(rest[i]) {
        i += 1;
    }
    if i != rest.len() {
        return false;
    }
    // hex 段不足 8 位才算「前缀」；已满 8 位就是完整占位符，可直接处理
    i - hex_start < 8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Mutex<PlaceholderMap>>, String) {
        let mut m = PlaceholderMap::with_key([3u8; 32], 100);
        let p = m.placeholder_for("alice@corp.com", "EMAIL");
        (Arc::new(Mutex::new(m)), p)
    }

    #[test]
    fn restores_split_placeholder() {
        let (map, p) = setup();
        let text = format!("data: {{\"delta\":\"hi {p} bye\"}}\n\n");
        // 在占位符中间任意位置切开都应正确还原
        for cut in 1..text.len() {
            let mut r = StreamRestorer::new(map.clone(), true);
            let mut out = r.feed(&text.as_bytes()[..cut]);
            out.extend(r.feed(&text.as_bytes()[cut..]));
            out.extend(r.finish());
            assert_eq!(
                String::from_utf8(out).unwrap(),
                "data: {\"delta\":\"hi alice@corp.com bye\"}\n\n",
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn byte_by_byte() {
        let (map, p) = setup();
        let text = format!("x{p}y{p}z");
        let mut r = StreamRestorer::new(map, false);
        let mut out = Vec::new();
        for b in text.as_bytes() {
            out.extend(r.feed(&[*b]));
        }
        out.extend(r.finish());
        assert_eq!(String::from_utf8(out).unwrap(), "xalice@corp.comyalice@corp.comz");
    }

    #[test]
    fn plain_text_passes_through_without_delay_when_no_prefix() {
        let (map, _) = setup();
        let mut r = StreamRestorer::new(map, false);
        let out = r.feed(b"hello world");
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn prefix_detection() {
        assert!(is_placeholder_prefix(b"P"));
        assert!(is_placeholder_prefix(b"PG_"));
        assert!(is_placeholder_prefix(b"PG_EMA"));
        assert!(is_placeholder_prefix(b"PG_EMAIL_"));
        assert!(is_placeholder_prefix(b"PG_EMAIL_abc1"));
        assert!(!is_placeholder_prefix(b"PG_EMAIL_abc12345"));
        assert!(!is_placeholder_prefix(b"PX"));
        assert!(!is_placeholder_prefix(b"PG_email"));
    }
}
