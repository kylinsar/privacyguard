//! 模式 B：系统 PAC（networksetup）设置与恢复。

use crate::shell::{run, run_as_admin, sh_quote};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PAC_STATE_KEY: &str = "system_pac";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceProxyState {
    pub service: String,
    pub url: Option<String>,
    pub enabled: bool,
}

/// 启用系统 PAC 前保存的原始状态，用于恢复。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PacBackup {
    pub pac_url: String,
    pub services: Vec<ServiceProxyState>,
    pub applied_at: String,
}

/// 列出已启用的网络服务（跳过带 `*` 前缀的禁用项）。
pub fn list_services() -> Result<Vec<String>> {
    let out = run("networksetup", &["-listallnetworkservices"])?;
    Ok(out
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('*'))
        .map(str::to_string)
        .collect())
}

pub fn get_autoproxy(service: &str) -> Result<ServiceProxyState> {
    let out = run("networksetup", &["-getautoproxyurl", service])?;
    let mut url = None;
    let mut enabled = false;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("URL:") {
            let v = v.trim();
            if !v.is_empty() && v != "(null)" {
                url = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("Enabled:") {
            enabled = v.trim().eq_ignore_ascii_case("yes");
        }
    }
    Ok(ServiceProxyState {
        service: service.to_string(),
        url,
        enabled,
    })
}

pub fn snapshot() -> Result<Vec<ServiceProxyState>> {
    list_services()?
        .iter()
        .map(|s| get_autoproxy(s))
        .collect()
}

/// 对所有启用的网络服务设置 PAC（一次管理员授权）。返回备份。
pub fn apply_pac(pac_url: &str) -> Result<PacBackup> {
    let services = snapshot()?;
    anyhow::ensure!(!services.is_empty(), "没有找到可用的网络服务");
    let mut script = String::new();
    for s in &services {
        script.push_str(&format!(
            "networksetup -setautoproxyurl {svc} {url} && networksetup -setautoproxystate {svc} on; ",
            svc = sh_quote(&s.service),
            url = sh_quote(pac_url)
        ));
    }
    run_as_admin(&script, "PrivacyGuard 需要管理员权限设置系统自动代理配置（PAC）。")
        .context("设置系统 PAC 失败")?;
    Ok(PacBackup {
        pac_url: pac_url.to_string(),
        services,
        applied_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// 按备份恢复各服务原始 PAC 设置。
pub fn restore_pac(backup: &PacBackup) -> Result<()> {
    let mut script = String::new();
    for s in &backup.services {
        let svc = sh_quote(&s.service);
        match (&s.url, s.enabled) {
            (Some(url), true) => script.push_str(&format!(
                "networksetup -setautoproxyurl {svc} {} && networksetup -setautoproxystate {svc} on; ",
                sh_quote(url)
            )),
            (Some(url), false) => script.push_str(&format!(
                "networksetup -setautoproxyurl {svc} {} ; networksetup -setautoproxystate {svc} off; ",
                sh_quote(url)
            )),
            (None, _) => script.push_str(&format!("networksetup -setautoproxystate {svc} off; ")),
        }
    }
    if script.is_empty() {
        return Ok(());
    }
    run_as_admin(&script, "PrivacyGuard 需要管理员权限恢复系统代理设置。")
        .context("恢复系统 PAC 失败")?;
    Ok(())
}

/// 当前系统是否仍指向我们的 PAC（用于启动自检 / 状态展示）。
pub fn is_pac_active(pac_url: &str) -> bool {
    snapshot()
        .map(|v| v.iter().any(|s| s.enabled && s.url.as_deref() == Some(pac_url)))
        .unwrap_or(false)
}
