//! 桌面端与 `pg` 启动器之间的运行时信息交换（监听地址、PID）。

use crate::paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub pid: u32,
    pub listen: SocketAddr,
    pub started_at: String,
    pub ca_fingerprint: String,
}

pub fn write(info: &RuntimeInfo) -> Result<()> {
    let p = paths::runtime_file();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(info)?)?;
    Ok(())
}

pub fn clear() {
    let _ = std::fs::remove_file(paths::runtime_file());
}

/// 读取并校验：进程仍存活且端口可连。
pub fn read_live() -> Option<RuntimeInfo> {
    let s = std::fs::read_to_string(paths::runtime_file()).ok()?;
    let info: RuntimeInfo = serde_json::from_str(&s).ok()?;
    if !pid_alive(info.pid) {
        return None;
    }
    std::net::TcpStream::connect_timeout(&info.listen, std::time::Duration::from_millis(300)).ok()?;
    Some(info)
}

fn pid_alive(pid: u32) -> bool {
    // kill -0 语义
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
