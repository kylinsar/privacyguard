//! 模式 C：把 `pg` 启动器安装到 PATH（/usr/local/bin 符号链接）。

use crate::shell::{run_as_admin, sh_quote};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const LINK_DIR: &str = "/usr/local/bin";
pub const LINK_NAMES: &[&str] = &["pg", "pg-claude", "pg-codex"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliStatus {
    pub installed: bool,
    pub link_dir: String,
    pub target: Option<String>,
    pub links: Vec<(String, bool)>,
}

pub fn status(target: &Path) -> CliStatus {
    let mut links = Vec::new();
    let mut all = true;
    for n in LINK_NAMES {
        let p = Path::new(LINK_DIR).join(n);
        let ok = std::fs::read_link(&p)
            .map(|t| t == target)
            .unwrap_or(false);
        all &= ok;
        links.push((n.to_string(), ok));
    }
    CliStatus {
        installed: all,
        link_dir: LINK_DIR.into(),
        target: Some(target.to_string_lossy().to_string()),
        links,
    }
}

pub fn install(target: &Path) -> Result<()> {
    let mut script = format!("mkdir -p {}; ", sh_quote(LINK_DIR));
    for n in LINK_NAMES {
        script.push_str(&format!(
            "ln -sfn {} {}; ",
            sh_quote(&target.to_string_lossy()),
            sh_quote(&format!("{LINK_DIR}/{n}"))
        ));
    }
    run_as_admin(&script, "PrivacyGuard 需要管理员权限在 /usr/local/bin 安装 pg 命令行工具。")?;
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut script = String::new();
    for n in LINK_NAMES {
        script.push_str(&format!("rm -f {}; ", sh_quote(&format!("{LINK_DIR}/{n}"))));
    }
    run_as_admin(&script, "PrivacyGuard 需要管理员权限移除 /usr/local/bin 中的 pg 命令。")?;
    Ok(())
}
