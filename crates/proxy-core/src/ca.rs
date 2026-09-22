//! 根 CA 的生成 / 加载，以及按 SNI 动态签发叶证书（带缓存）。

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

pub const CA_CERT_FILE: &str = "pg-root-ca.pem";
pub const CA_KEY_FILE: &str = "pg-root-ca.key.pem";
pub const CA_COMMON_NAME: &str = "PrivacyGuard Local Root CA";
const CA_VALID_YEARS: i64 = 10;
const LEAF_VALID_DAYS: i64 = 30;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaInfo {
    pub common_name: String,
    pub fingerprint_sha256: String,
    pub not_before: String,
    pub not_after: String,
    pub cert_path: PathBuf,
}

pub struct CertificateAuthority {
    cert_pem: String,
    cert_der: CertificateDer<'static>,
    issuer: Issuer<'static, KeyPair>,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    cert_path: PathBuf,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for CertificateAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertificateAuthority")
            .field("fingerprint", &self.fingerprint_sha256())
            .finish()
    }
}

impl CertificateAuthority {
    /// 从目录加载，不存在则生成并写入（私钥 0600）。
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("创建 CA 目录 {}", dir.display()))?;
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            let cert_pem = fs::read_to_string(&cert_path)?;
            let key_pem = fs::read_to_string(&key_path)?;
            match Self::from_pem(&cert_pem, &key_pem, cert_path.clone()) {
                Ok(ca) => return Ok(ca),
                Err(e) => tracing::warn!("已有 CA 无法加载，将重新生成: {e:#}"),
            }
        }
        let ca = Self::generate(cert_path.clone())?;
        fs::write(&cert_path, ca.cert_pem())?;
        write_private(&key_path, ca.key_pem().as_bytes())?;
        Ok(ca)
    }

    /// 重新生成（吊销旧 CA）。
    pub fn regenerate(dir: &Path) -> Result<Self> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);
        let _ = fs::remove_file(&cert_path);
        let _ = fs::remove_file(&key_path);
        Self::load_or_create(dir)
    }

    fn generate(cert_path: PathBuf) -> Result<Self> {
        let key = KeyPair::generate().context("生成 CA 密钥")?;
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, CA_COMMON_NAME);
        dn.push(DnType::OrganizationName, "PrivacyGuard");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(365 * CA_VALID_YEARS);
        let cert = params.self_signed(&key).context("自签 CA")?;
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();
        Self::from_pem(&cert_pem, &key_pem, cert_path)
    }

    fn from_pem(cert_pem: &str, key_pem: &str, cert_path: PathBuf) -> Result<Self> {
        let key = KeyPair::from_pem(key_pem).context("解析 CA 私钥")?;
        let issuer = Issuer::from_ca_cert_pem(cert_pem, key).context("解析 CA 证书")?;
        let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .context("CA PEM 中无证书")??;
        let (not_before, not_after) = cert_validity(&cert_der)?;
        Ok(Self {
            cert_pem: cert_pem.to_string(),
            cert_der,
            issuer,
            not_before,
            not_after,
            cert_path,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    pub fn key_pem(&self) -> String {
        self.issuer.key().serialize_pem()
    }

    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    pub fn fingerprint_sha256(&self) -> String {
        let digest = Sha256::digest(self.cert_der.as_ref());
        hex::encode_upper(digest)
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(":")
    }

    pub fn info(&self) -> CaInfo {
        CaInfo {
            common_name: CA_COMMON_NAME.to_string(),
            fingerprint_sha256: self.fingerprint_sha256(),
            not_before: self.not_before.to_string(),
            not_after: self.not_after.to_string(),
            cert_path: self.cert_path.clone(),
        }
    }

    /// 为 host 签发（或取缓存）叶证书。
    pub fn leaf_for(&self, host: &str) -> Result<Arc<CertifiedKey>> {
        let host = host.to_ascii_lowercase();
        if let Some(ck) = self.cache.lock().get(&host) {
            return Ok(ck.clone());
        }
        let ck = Arc::new(self.issue_leaf(&host)?);
        self.cache.lock().insert(host, ck.clone());
        Ok(ck)
    }

    fn issue_leaf(&self, host: &str) -> Result<CertifiedKey> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        params.subject_alt_names = vec![match host.parse::<std::net::IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(host.try_into().context("host 不是合法 DNS 名")?),
        }];
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(LEAF_VALID_DAYS);
        let cert = params.signed_by(&key, &self.issuer)?;

        let chain = vec![cert.der().clone(), self.cert_der.clone()];
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let provider = rustls::crypto::ring::default_provider();
        let signing_key = provider
            .key_provider
            .load_private_key(key_der)
            .map_err(|e| anyhow::anyhow!("加载叶证书私钥: {e}"))?;
        Ok(CertifiedKey::new(chain, signing_key))
    }

    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }
}

impl ResolvesServerCert for CertificateAuthority {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let host = client_hello.server_name()?;
        match self.leaf_for(host) {
            Ok(ck) => Some(ck),
            Err(e) => {
                tracing::error!("签发 {host} 叶证书失败: {e:#}");
                None
            }
        }
    }
}

fn cert_validity(der: &CertificateDer<'_>) -> Result<(OffsetDateTime, OffsetDateTime)> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(der.as_ref()).context("解析 X.509")?;
    let nb = OffsetDateTime::from_unix_timestamp(cert.validity().not_before.timestamp())?;
    let na = OffsetDateTime::from_unix_timestamp(cert.validity().not_after.timestamp())?;
    Ok((nb, na))
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_load_and_issue() {
        let dir = std::env::temp_dir().join(format!("pg-ca-test-{}", uuid::Uuid::new_v4()));
        let ca = CertificateAuthority::load_or_create(&dir).unwrap();
        let fp = ca.fingerprint_sha256();
        let ca2 = CertificateAuthority::load_or_create(&dir).unwrap();
        assert_eq!(fp, ca2.fingerprint_sha256());
        let leaf = ca2.leaf_for("api.openai.com").unwrap();
        assert_eq!(leaf.cert.len(), 2);
        let again = ca2.leaf_for("API.openai.com").unwrap();
        assert!(Arc::ptr_eq(&leaf, &again));
        let _ = fs::remove_dir_all(dir);
    }
}
