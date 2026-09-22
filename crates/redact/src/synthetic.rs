//! 拟真替换值生成：把检测到的敏感值换成「同类型、看起来真实」的假值，
//! 而不是 `PG_EMAIL_xxxx` 这种一眼可见的占位符。
//!
//! 所有生成都由 HMAC 摘要驱动，因此同一会话内同一原值总得到同一假值。
//! 生成结果需要与原值不同；调用方负责处理与已有映射的冲突。

/// 从摘要字节流按需取伪随机数。
struct Rng<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Rng<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn next(&mut self) -> u8 {
        let b = self.bytes[self.pos % self.bytes.len()];
        // 用位置扰动，避免摘要耗尽后简单循环
        let salt = (self.pos / self.bytes.len()) as u8;
        self.pos += 1;
        b.wrapping_add(salt.wrapping_mul(97))
    }
    fn below(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        (((self.next() as usize) << 8) | self.next() as usize) % n
    }
    fn digit(&mut self) -> char {
        char::from(b'0' + self.below(10) as u8)
    }
}

/// 能拟真生成的实体类型。
pub fn supports(entity: &str) -> bool {
    matches!(entity, "EMAIL" | "PHONE" | "ID_CARD" | "CARD" | "IP")
}

/// 生成拟真值。`attempt` 用于冲突时换一个结果。返回 None 表示不支持该类型。
pub fn generate(entity: &str, value: &str, digest: &[u8], attempt: usize) -> Option<String> {
    let mut rng = Rng::new(digest);
    for _ in 0..attempt {
        rng.next();
    }
    let out = match entity {
        "EMAIL" => email(&mut rng),
        "PHONE" => phone(value, &mut rng),
        "ID_CARD" => id_card(value, &mut rng),
        "CARD" => card(value, &mut rng),
        "IP" => ip(&mut rng),
        _ => return None,
    };
    Some(out)
}

const FIRST: &[&str] = &[
    "alex", "chris", "jamie", "taylor", "morgan", "jordan", "casey", "riley", "sam", "lee", "wei", "ming", "jun",
    "yan", "hao", "lin", "xin", "yu", "kai", "rui",
];
const LAST: &[&str] = &[
    "wang", "li", "zhang", "liu", "chen", "yang", "huang", "zhao", "wu", "zhou", "smith", "jones", "brown",
    "miller", "davis", "wilson", "moore", "clark", "lewis", "walker",
];
const DOMAINS: &[&str] = &[
    "gmail.com", "outlook.com", "hotmail.com", "163.com", "qq.com", "foxmail.com", "126.com", "icloud.com",
    "yahoo.com", "proton.me",
];

fn email(rng: &mut Rng) -> String {
    let f = FIRST[rng.below(FIRST.len())];
    let l = LAST[rng.below(LAST.len())];
    let d = DOMAINS[rng.below(DOMAINS.len())];
    match rng.below(4) {
        0 => format!("{f}.{l}@{d}"),
        1 => format!("{f}{l}{}@{d}", 10 + rng.below(90)),
        2 => format!("{l}.{f}{}@{d}", rng.below(100)),
        _ => format!("{f}_{l}@{d}"),
    }
}

/// 保留原有分隔符、国家码与前几位（看起来仍是同一地区的号码），替换其余数字。
fn phone(value: &str, rng: &mut Rng) -> String {
    let digits = value.chars().filter(|c| c.is_ascii_digit()).count();
    // 保留国家码 + 前 3 位数字（如 +86 138），其余替换
    let mut keep = if digits >= 8 { 3 } else { 1 };
    if let Some(rest) = value.strip_prefix('+') {
        keep += rest.chars().take_while(|c| c.is_ascii_digit()).count();
    }
    let mut seen = 0;
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_ascii_digit() {
            seen += 1;
            if seen <= keep {
                out.push(c);
            } else {
                out.push(rng.digit());
            }
        } else {
            out.push(c);
        }
    }
    if out == value {
        // 全部被保留（极短号码），强制改最后一位
        if let Some(last) = out.pop() {
            let d = last.to_digit(10).unwrap_or(0);
            out.push(char::from_digit((d + 1) % 10, 10).unwrap());
        }
    }
    out
}

/// 18 位身份证：保留地区码前 6 位，生成合法出生日期与校验位。
fn id_card(value: &str, rng: &mut Rng) -> String {
    let region: String = value.chars().take(6).collect();
    let year = 1960 + rng.below(45);
    let month = 1 + rng.below(12);
    let day = 1 + rng.below(28);
    let seq = format!("{:03}", rng.below(999));
    let body = format!("{region}{year:04}{month:02}{day:02}{seq}");
    let check = id_card_check(&body);
    format!("{body}{check}")
}

fn id_card_check(body17: &str) -> char {
    const W: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const C: [char; 11] = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let sum: u32 = body17
        .chars()
        .filter_map(|c| c.to_digit(10))
        .zip(W.iter())
        .map(|(d, w)| d * w)
        .sum();
    C[(sum % 11) as usize]
}

/// 银行卡：保留分隔符、前 6 位 BIN 与长度，其余随机并补 Luhn 校验位。
fn card(value: &str, rng: &mut Rng) -> String {
    let digits: Vec<char> = value.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len();
    let mut new: Vec<char> = Vec::with_capacity(n);
    for (i, c) in digits.iter().enumerate() {
        if i < 6 || i + 1 == n {
            new.push(*c);
        } else {
            new.push(rng.digit());
        }
    }
    // 计算校验位
    if n >= 2 {
        let check = luhn_check_digit(&new[..n - 1]);
        new[n - 1] = check;
    }
    let mut it = new.into_iter();
    value
        .chars()
        .map(|c| if c.is_ascii_digit() { it.next().unwrap_or(c) } else { c })
        .collect()
}

fn luhn_check_digit(body: &[char]) -> char {
    let mut sum = 0;
    for (i, c) in body.iter().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap_or(0);
        if i % 2 == 0 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    char::from_digit((10 - sum % 10) % 10, 10).unwrap()
}

fn ip(rng: &mut Rng) -> String {
    // 避开 0/10/127/169.254/172.16-31/192.168/224+ 等特殊段
    let first = loop {
        let a = 1 + rng.below(222);
        if !matches!(a, 10 | 127 | 169 | 172 | 192) {
            break a;
        }
    };
    format!("{first}.{}.{}.{}", rng.below(256), rng.below(256), 1 + rng.below(254))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> Vec<u8> {
        (0..32u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect()
    }

    #[test]
    fn phone_keeps_layout() {
        let d = digest();
        let out = generate("PHONE", "+86 138 0013 8000", &d, 0).unwrap();
        assert!(out.starts_with("+86 138 "));
        assert_eq!(out.len(), "+86 138 0013 8000".len());
        assert_ne!(out, "+86 138 0013 8000");
        let cn = generate("PHONE", "13800138000", &d, 0).unwrap();
        assert!(cn.starts_with("138") && cn.len() == 11);
    }

    #[test]
    fn card_is_luhn_valid() {
        let out = generate("CARD", "4111 1111 1111 1111", &digest(), 0).unwrap();
        assert_eq!(out.len(), 19);
        assert!(out.starts_with("4111 11"));
        let ds: Vec<u32> = out.chars().filter_map(|c| c.to_digit(10)).collect();
        let mut sum = 0;
        for (i, d) in ds.iter().rev().enumerate() {
            let mut d = *d;
            if i % 2 == 1 {
                d *= 2;
                if d > 9 {
                    d -= 9;
                }
            }
            sum += d;
        }
        assert_eq!(sum % 10, 0);
    }

    #[test]
    fn id_card_checksum() {
        let out = generate("ID_CARD", "110101199003072316", &digest(), 0).unwrap();
        assert_eq!(out.len(), 18);
        assert!(out.starts_with("110101"));
        assert_eq!(id_card_check(&out[..17]), out.chars().last().unwrap());
        // 已知合法样例
        assert_eq!(id_card_check("11010519491231002"), 'X');
    }

    #[test]
    fn deterministic_and_attempt_varies() {
        let d = digest();
        let a = generate("EMAIL", "x@y.z", &d, 0).unwrap();
        let b = generate("EMAIL", "x@y.z", &d, 0).unwrap();
        let c = generate("EMAIL", "x@y.z", &d, 1).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.contains('@'));
        assert!(generate("SECRET", "x", &d, 0).is_none());
    }
}
