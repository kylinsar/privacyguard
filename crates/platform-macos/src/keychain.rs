//! CA 证书在 macOS 钥匙串中的安装 / 检测 / 移除。

use crate::shell::{run, run_as_admin, run_lenient, sh_quote};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeychainKind {
    Login,
    System,
}

impl KeychainKind {
    fn path(&self) -> String {
        match self {
            KeychainKind::Login => crate::paths::home()
                .join("Library/Keychains/login.keychain-db")
                .to_string_lossy()
                .to_string(),
            KeychainKind::System => "/Library/Keychains/System.keychain".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustStatus {
    pub in_login_keychain: bool,
    pub in_system_keychain: bool,
    /// `security verify-cert` 认为该 CA 可作为 SSL 信任锚
    pub trusted_for_ssl: bool,
    pub sha1: String,
    pub detail: String,
}

fn sha1_hex(der: &[u8]) -> String {
    hex::encode_upper(Sha1::digest(der))
}

fn keychain_contains(kind: KeychainKind, sha1: &str) -> bool {
    let (ok, out) = run_lenient("security", &["find-certificate", "-a", "-Z", &kind.path()]);
    if !ok {
        return false;
    }
    out.to_ascii_uppercase().contains(&format!("SHA-1 HASH: {sha1}"))
}

pub fn status(cert_pem_path: &Path, cert_der: &[u8]) -> TrustStatus {
    let sha1 = sha1_hex(cert_der);
    let in_login = keychain_contains(KeychainKind::Login, &sha1);
    let in_system = keychain_contains(KeychainKind::System, &sha1);
    let (trusted, detail) = run_lenient(
        "security",
        &["verify-cert", "-c", &cert_pem_path.to_string_lossy(), "-p", "ssl", "-L"],
    );
    TrustStatus {
        in_login_keychain: in_login,
        in_system_keychain: in_system,
        trusted_for_ssl: trusted,
        sha1,
        detail: detail.trim().to_string(),
    }
}

/// 安装并设为受信任根。登录钥匙串会弹出用户密码框；系统钥匙串需要管理员授权。
pub fn install(kind: KeychainKind, cert_pem_path: &Path) -> Result<()> {
    let pem = cert_pem_path.to_string_lossy().to_string();
    match kind {
        KeychainKind::Login => {
            run(
                "security",
                &["add-trusted-cert", "-r", "trustRoot", "-k", &kind.path(), &pem],
            )
            .context("安装到登录钥匙串失败")?;
        }
        KeychainKind::System => {
            let script = format!(
                "security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
                sh_quote(&pem)
            );
            run_as_admin(&script, "PrivacyGuard 需要管理员权限将本地 CA 证书安装到系统钥匙串。")?;
        }
    }
    Ok(())
}

pub fn remove(kind: KeychainKind, cert_pem_path: &Path, cert_der: &[u8]) -> Result<()> {
    let sha1 = sha1_hex(cert_der);
    let pem = cert_pem_path.to_string_lossy().to_string();
    match kind {
        KeychainKind::Login => {
            // 先移除用户域信任设置再删除证书；信任设置不存在时忽略错误
            let _ = run_lenient("security", &["remove-trusted-cert", &pem]);
            run("security", &["delete-certificate", "-Z", &sha1, &kind.path()])
                .context("从登录钥匙串删除失败")?;
        }
        KeychainKind::System => {
            let script = format!(
                "security remove-trusted-cert -d {pem}; security delete-certificate -Z {sha1} /Library/Keychains/System.keychain",
                pem = sh_quote(&pem)
            );
            run_as_admin(&script, "PrivacyGuard 需要管理员权限从系统钥匙串移除本地 CA 证书。")?;
        }
    }
    Ok(())
}
