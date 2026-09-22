//! 三种保护模式的启用 / 关闭 / 自检。
//!
//! - A `Config`：写 Claude `settings.json` / Codex `config.toml`
//! - B `SystemProxy`：系统 PAC（按已启用工具收窄域名）+ Claude 额外写 settings.json
//! - C `Launcher`：仅标记，依赖 `pg` 启动器

use crate::state::AppState;
use anyhow::{anyhow, bail, Context, Result};
use platform_macos::agents::{self, ConfigRollback};
use platform_macos::{cli_install, keychain, network};
use serde::Serialize;
use store::{Agent, AgentActivation, ProtectionMode, Store};

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub info: agents::AgentInfo,
    pub activation: AgentActivation,
    /// 该工具在当前状态下的提示（前置条件缺失、风险说明）
    pub notices: Vec<String>,
}

fn agent_hosts(agent: Agent) -> Vec<String> {
    match agent {
        Agent::Claude => vec!["api.anthropic.com".into()],
        Agent::Codex => vec!["api.openai.com".into(), "chatgpt.com".into()],
    }
}

/// 模式 B 下 PAC 应包含的域名集合；无人使用 B 时返回 None。
pub fn pac_hosts(store: &Store) -> Result<Option<Vec<String>>> {
    let mut hosts = Vec::new();
    for a in Agent::all() {
        let act = store.activation(a)?;
        if act.enabled && act.mode == Some(ProtectionMode::SystemProxy) {
            hosts.extend(agent_hosts(a));
        }
    }
    Ok(if hosts.is_empty() { None } else { Some(hosts) })
}

fn ca_trusted(state: &AppState) -> bool {
    let ca = state.ca();
    let st = keychain::status(ca.cert_path(), ca.cert_der().as_ref());
    st.trusted_for_ssl
}

pub fn view(state: &AppState, agent: Agent) -> Result<AgentView> {
    let info = agents::detect(agent);
    let activation = state.store.activation(agent)?;
    let mut notices = Vec::new();
    if !info.installed {
        notices.push(format!("未检测到 {} 可执行文件", agent.key()));
    }
    if !info.conflicts.is_empty() && !activation.enabled {
        notices.extend(info.conflicts.iter().map(|c| format!("配置冲突：{c}")));
    }
    if !ca_trusted(state) {
        notices.push("CA 证书尚未被系统信任：模式 B 需要先安装证书；模式 A/C 通过环境变量下发 CA，可不安装".into());
    }
    if agent == Agent::Codex {
        notices.push("Codex 可能使用 WebSocket 传输（Responses API），首版对 WebSocket 仅透传不脱敏".into());
        if activation.mode == Some(ProtectionMode::Config) {
            notices.push("模式 A 通过 openai_base_url / chatgpt_base_url 指向本地反向入口；ChatGPT 登录态下需实测".into());
        }
    }
    if activation.enabled && activation.mode == Some(ProtectionMode::SystemProxy) {
        if let Ok(pac) = state.pac_url() {
            if !network::is_pac_active(&pac) {
                notices.push("系统 PAC 当前未指向 PrivacyGuard（可能被网络切换或其他工具覆盖），请重新应用".into());
            }
        }
    }
    if activation.enabled && activation.mode == Some(ProtectionMode::Launcher) {
        if let Some(bin) = crate::state::pg_binary_path() {
            if !cli_install::status(&bin).installed {
                notices.push("pg 命令行工具尚未安装到 /usr/local/bin，请在设置页安装".into());
            }
        }
    }
    Ok(AgentView {
        info,
        activation,
        notices,
    })
}

pub fn enable(state: &AppState, agent: Agent, mode: ProtectionMode) -> Result<AgentActivation> {
    let store = &state.store;
    let current = store.activation(agent)?;
    if current.enabled {
        // 切换模式：先关闭再开启
        disable(state, agent)?;
    }
    let proxy_url = state.proxy_url()?;
    let ca_path = state.ca().cert_path().to_path_buf();
    let mut act = AgentActivation {
        enabled: true,
        mode: Some(mode),
        rollback: serde_json::Value::Null,
        activated_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    match (agent, mode) {
        (Agent::Claude, ProtectionMode::Config) => {
            let rb = agents::claude_enable(&proxy_url, &ca_path).context("写入 ~/.claude/settings.json")?;
            act.rollback = serde_json::to_value(rb)?;
        }
        (Agent::Codex, ProtectionMode::Config) => {
            let rb = agents::codex_enable(&proxy_url).context("写入 ~/.codex/config.toml")?;
            act.rollback = serde_json::to_value(rb)?;
        }
        (_, ProtectionMode::SystemProxy) => {
            if !ca_trusted(state) {
                bail!("模式 B 需要 CA 证书已被系统信任，请先在「证书」页安装");
            }
            // Claude Code 不读系统代理，额外写 settings.json
            let mut claude_rb: Option<ConfigRollback> = None;
            if agent == Agent::Claude {
                let rb = agents::claude_enable(&proxy_url, &ca_path)?;
                act.rollback = serde_json::to_value(&rb)?;
                claude_rb = Some(rb);
            }
            // 先把新激活状态写入，才能算出 PAC 域名集合
            store.set_activation(agent, &act)?;
            state.push_config()?;
            if let Err(e) = apply_system_pac(state) {
                // 失败回滚：恢复激活状态与已写入的 Claude 配置
                let _ = store.set_activation(agent, &current);
                if let Some(rb) = &claude_rb {
                    let _ = agents::claude_disable(rb);
                }
                let _ = state.push_config();
                return Err(e);
            }
        }
        (_, ProtectionMode::Launcher) => {}
    }
    store.set_activation(agent, &act)?;
    state.push_config()?;
    Ok(act)
}

pub fn disable(state: &AppState, agent: Agent) -> Result<AgentActivation> {
    let store = &state.store;
    let current = store.activation(agent)?;
    if !current.enabled {
        return Ok(current);
    }
    match (agent, current.mode) {
        (Agent::Claude, Some(ProtectionMode::Config) | Some(ProtectionMode::SystemProxy)) => {
            if let Ok(rb) = serde_json::from_value::<ConfigRollback>(current.rollback.clone()) {
                agents::claude_disable(&rb).context("回滚 ~/.claude/settings.json")?;
            }
        }
        (Agent::Codex, Some(ProtectionMode::Config)) => {
            if let Ok(rb) = serde_json::from_value::<ConfigRollback>(current.rollback.clone()) {
                agents::codex_disable(&rb).context("回滚 ~/.codex/config.toml")?;
            }
        }
        _ => {}
    }
    let off = AgentActivation {
        enabled: false,
        mode: current.mode,
        rollback: serde_json::Value::Null,
        activated_at: None,
    };
    store.set_activation(agent, &off)?;
    state.push_config()?;
    if current.mode == Some(ProtectionMode::SystemProxy) {
        // 若已无人使用 B，则恢复系统 PAC；否则重新应用以收窄域名
        if pac_hosts(store)?.is_none() {
            restore_system_pac(state)?;
        } else {
            apply_system_pac(state)?;
        }
    }
    Ok(off)
}

/// 应用（或重新应用）系统 PAC。首次应用保存备份。
pub fn apply_system_pac(state: &AppState) -> Result<network::PacBackup> {
    let pac_url = state.pac_url()?;
    let existing: Option<network::PacBackup> = state.store.get_state(network::PAC_STATE_KEY)?;
    let backup = network::apply_pac(&pac_url)?;
    let keep = match existing {
        // 已有备份说明之前就是我们改的，保留最初的备份，避免把自己的 PAC 记成「原始值」
        Some(b) => b,
        None => backup,
    };
    state.store.set_state(network::PAC_STATE_KEY, &keep)?;
    Ok(keep)
}

pub fn restore_system_pac(state: &AppState) -> Result<()> {
    let existing: Option<network::PacBackup> = state.store.get_state(network::PAC_STATE_KEY)?;
    if let Some(b) = existing {
        network::restore_pac(&b)?;
        state.store.delete_state(network::PAC_STATE_KEY)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PacStatus {
    pub managed: bool,
    pub active: bool,
    pub pac_url: String,
    pub hosts: Vec<String>,
    pub services: Vec<network::ServiceProxyState>,
}

pub fn pac_status(state: &AppState) -> Result<PacStatus> {
    let pac_url = state.pac_url()?;
    let managed = state
        .store
        .get_state::<network::PacBackup>(network::PAC_STATE_KEY)?
        .is_some();
    Ok(PacStatus {
        managed,
        active: network::is_pac_active(&pac_url),
        pac_url,
        hosts: pac_hosts(&state.store)?.unwrap_or_default(),
        services: network::snapshot().unwrap_or_default(),
    })
}

/// 启动自检：配置被用户手动改掉则标记为关闭；清理过期日志。
pub fn reconcile(state: &AppState) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for a in Agent::all() {
        let act = state.store.activation(a)?;
        if !act.enabled {
            continue;
        }
        if matches!(act.mode, Some(ProtectionMode::Config)) {
            let info = agents::detect(a);
            if !info.config_managed_by_us {
                notes.push(format!("{} 的配置已被外部修改，保护已标记为关闭", a.key()));
                state.store.set_activation(
                    a,
                    &AgentActivation {
                        enabled: false,
                        mode: act.mode,
                        ..Default::default()
                    },
                )?;
            }
        }
    }
    let days = state.store.app_settings()?.log_retention_days;
    if days > 0 {
        let before = chrono::Utc::now() - chrono::Duration::days(days as i64);
        let n = state.store.purge_before(before)?;
        if n > 0 {
            notes.push(format!("已清理 {n} 条过期日志"));
        }
    }
    Ok(notes)
}

pub fn ensure_pg_binary() -> Result<std::path::PathBuf> {
    crate::state::pg_binary_path().ok_or_else(|| anyhow!("找不到随应用打包的 pg 启动器"))
}
