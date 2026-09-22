//! 进程执行工具：普通命令、登录 shell 查找、管理员提权。

use anyhow::{anyhow, Context, Result};
use std::process::Command;

pub fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("执行 {cmd} 失败"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(anyhow!(
            "{cmd} {} 退出码 {:?}: {}{}",
            args.join(" "),
            out.status.code(),
            stdout.trim(),
            stderr.trim()
        ))
    }
}

/// 允许失败，返回 (成功?, 合并输出)。
pub fn run_lenient(cmd: &str, args: &[&str]) -> (bool, String) {
    match Command::new(cmd).args(args).output() {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), s)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// 通过用户登录 shell 查找命令的绝对路径（GUI 应用的 PATH 往往不完整）。
pub fn which_via_login_shell(name: &str) -> Option<String> {
    if let Ok(p) = std::env::var("PATH") {
        for dir in p.split(':') {
            let candidate = std::path::Path::new(dir).join(name);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = Command::new(&shell)
        .args(["-lic", &format!("command -v {name}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .last()
        .map(str::to_string)
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 用 osascript 以管理员权限执行一段 shell 脚本（弹一次系统授权框）。
pub fn run_as_admin(script: &str, prompt: &str) -> Result<String> {
    let script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"{}\"",
        applescript_escape(script),
        applescript_escape(prompt)
    );
    let out = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("执行 osascript 失败")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if err.contains("-128") {
            Err(anyhow!("用户取消了授权"))
        } else {
            Err(anyhow!("管理员命令失败: {}", err.trim()))
        }
    }
}

/// shell 单引号转义
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
