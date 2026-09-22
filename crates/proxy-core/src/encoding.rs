//! 请求体内容编码解码。
//!
//! Codex 在走官方 Codex 后端时会把 `/responses` 请求体用 zstd 压缩（`Content-Encoding: zstd`），
//! 不解压就无法脱敏、也无法判断是否含隐私。这里统一把已知编码解开，脱敏后以 identity 转发。

use anyhow::{anyhow, bail, Result};
use std::io::Read;

/// 解压后体积上限，防御压缩炸弹。
pub const MAX_DECODED: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyEncoding {
    Identity,
    Zstd,
    Gzip,
    Deflate,
    Other,
}

pub fn parse(header: Option<&str>) -> BodyEncoding {
    let Some(v) = header else { return BodyEncoding::Identity };
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("identity") {
        BodyEncoding::Identity
    } else if v.eq_ignore_ascii_case("zstd") {
        BodyEncoding::Zstd
    } else if v.eq_ignore_ascii_case("gzip") || v.eq_ignore_ascii_case("x-gzip") {
        BodyEncoding::Gzip
    } else if v.eq_ignore_ascii_case("deflate") {
        BodyEncoding::Deflate
    } else {
        // 多重编码（`gzip, zstd`）或 br 等：不处理
        BodyEncoding::Other
    }
}

/// 解码请求体。`Identity` 原样返回；`Other` 返回错误。
pub fn decode(enc: BodyEncoding, body: &[u8]) -> Result<Vec<u8>> {
    match enc {
        BodyEncoding::Identity => Ok(body.to_vec()),
        BodyEncoding::Zstd => read_limited(zstd::stream::read::Decoder::new(body)?),
        BodyEncoding::Gzip => read_limited(flate2::read::MultiGzDecoder::new(body)),
        BodyEncoding::Deflate => read_limited(flate2::read::ZlibDecoder::new(body)),
        BodyEncoding::Other => bail!("不支持的 content-encoding"),
    }
}

fn read_limited<R: Read>(r: R) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    r.take(MAX_DECODED as u64 + 1).read_to_end(&mut out)?;
    if out.len() > MAX_DECODED {
        return Err(anyhow!("解压后超过 {} 字节上限", MAX_DECODED));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_roundtrip() {
        let raw = br#"{"input":"hello"}"#;
        let z = zstd::stream::encode_all(&raw[..], 3).unwrap();
        assert_eq!(decode(parse(Some("zstd")), &z).unwrap(), raw);
    }

    #[test]
    fn gzip_roundtrip() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let raw = b"{\"a\":1}";
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(raw).unwrap();
        let g = e.finish().unwrap();
        assert_eq!(decode(parse(Some("gzip")), &g).unwrap(), raw);
        assert_eq!(parse(Some("gzip, zstd")), BodyEncoding::Other);
        assert_eq!(parse(None), BodyEncoding::Identity);
    }
}
