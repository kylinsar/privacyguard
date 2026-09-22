//! Tauri 命令：前端可调用的全部接口。错误统一转为字符串。

use crate::activation::{self, AgentView, PacStatus};
use crate::state::{AppState, ProxyStatus};
use platform_macos::{agents, cli_install, keychain, paths, shell};
use proxy_core::CaInfo;
use redact::{Rule, Span};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use store::{
    Agent, AgentActivation, AppSettings, ModelPrice, Persona, ProtectionMode, RequestDetail, RequestFilter, RequestRow,
    Stats, UsageReport,
};
use tauri::State;

type R<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e}")
}
fn anyerr(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ---------- 代理 ----------

#[tauri::command]
pub fn proxy_status(state: State<'_, Arc<AppState>>) -> ProxyStatus {
    state.status()
}

#[tauri::command]
pub async fn proxy_start(state: State<'_, Arc<AppState>>) -> R<ProxyStatus> {
    state.start_proxy().await.map_err(anyerr)
}

#[tauri::command]
pub async fn proxy_stop(state: State<'_, Arc<AppState>>) -> R<ProxyStatus> {
    Ok(state.stop_proxy().await)
}

#[tauri::command]
pub async fn proxy_restart(state: State<'_, Arc<AppState>>) -> R<ProxyStatus> {
    state.restart_proxy().await.map_err(anyerr)
}

// ---------- 设置 ----------

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> R<AppSettings> {
    state.store.app_settings().map_err(anyerr)
}

#[tauri::command]
pub async fn save_settings(state: State<'_, Arc<AppState>>, settings: AppSettings) -> R<ProxyStatus> {
    let old = state.store.app_settings().map_err(anyerr)?;
    state.store.save_app_settings(&settings).map_err(anyerr)?;
    let needs_restart = old.port != settings.port || old.upstream_proxy != settings.upstream_proxy;
    if needs_restart && state.is_running() {
        state.restart_proxy().await.map_err(anyerr)?;
    } else {
        state.push_config().map_err(anyerr)?;
    }
    if old.active_persona != settings.active_persona || old.substitution_style != settings.substitution_style {
        state.reload_rules().map_err(anyerr)?;
    }
    Ok(state.status())
}

#[derive(Serialize)]
pub struct Paths {
    pub data_dir: String,
    pub db_path: String,
    pub ca_dir: String,
    pub backups_dir: String,
    pub claude_settings: String,
    pub codex_config: String,
}

#[tauri::command]
pub fn app_paths() -> Paths {
    Paths {
        data_dir: paths::app_data_dir().to_string_lossy().into(),
        db_path: paths::db_path().to_string_lossy().into(),
        ca_dir: paths::ca_dir().to_string_lossy().into(),
        backups_dir: paths::backups_dir().to_string_lossy().into(),
        claude_settings: paths::claude_settings_path().to_string_lossy().into(),
        codex_config: paths::codex_config_path().to_string_lossy().into(),
    }
}

#[tauri::command]
pub fn reveal_path(path: String) -> R<()> {
    shell::run("open", &["-R", &path]).map(|_| ()).map_err(anyerr)
}

// ---------- 证书 ----------

#[derive(Serialize)]
pub struct CaView {
    pub info: CaInfo,
    pub trust: keychain::TrustStatus,
    pub pem: String,
}

#[tauri::command]
pub fn ca_view(state: State<'_, Arc<AppState>>) -> CaView {
    let ca = state.ca();
    CaView {
        info: ca.info(),
        trust: keychain::status(ca.cert_path(), ca.cert_der().as_ref()),
        pem: ca.cert_pem().to_string(),
    }
}

#[tauri::command]
pub async fn ca_install(state: State<'_, Arc<AppState>>, kind: keychain::KeychainKind) -> R<keychain::TrustStatus> {
    let ca = state.ca();
    let path = ca.cert_path().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || keychain::install(kind, &path))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    Ok(keychain::status(ca.cert_path(), ca.cert_der().as_ref()))
}

#[tauri::command]
pub async fn ca_remove(state: State<'_, Arc<AppState>>, kind: keychain::KeychainKind) -> R<keychain::TrustStatus> {
    let ca = state.ca();
    let der = ca.cert_der().as_ref().to_vec();
    let path = ca.cert_path().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || keychain::remove(kind, &path, &der))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    Ok(keychain::status(ca.cert_path(), ca.cert_der().as_ref()))
}

#[tauri::command]
pub async fn ca_regenerate(state: State<'_, Arc<AppState>>) -> R<CaView> {
    // 先尽量把旧证书从钥匙串移除
    let old = state.ca();
    let der = old.cert_der().as_ref().to_vec();
    let path = old.cert_path().to_path_buf();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        keychain::remove(keychain::KeychainKind::Login, &path, &der)
    })
    .await;
    state.regenerate_ca().await.map_err(anyerr)?;
    Ok(ca_view(state))
}

#[tauri::command]
pub fn ca_export(state: State<'_, Arc<AppState>>, dest: String) -> R<()> {
    std::fs::write(&dest, state.ca().cert_pem()).map_err(err)
}

// ---------- 保护 ----------

#[tauri::command]
pub fn agent_view(state: State<'_, Arc<AppState>>, agent: Agent) -> R<AgentView> {
    activation::view(&state, agent).map_err(anyerr)
}

#[tauri::command]
pub async fn agent_enable(
    state: State<'_, Arc<AppState>>,
    agent: Agent,
    mode: ProtectionMode,
) -> R<AgentActivation> {
    if !state.is_running() {
        state.start_proxy().await.map_err(anyerr)?;
    }
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || activation::enable(&st, agent, mode))
        .await
        .map_err(err)?
        .map_err(anyerr)
}

#[tauri::command]
pub async fn agent_disable(state: State<'_, Arc<AppState>>, agent: Agent) -> R<AgentActivation> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || activation::disable(&st, agent))
        .await
        .map_err(err)?
        .map_err(anyerr)
}

#[tauri::command]
pub fn agent_detect(agent: Agent) -> agents::AgentInfo {
    agents::detect(agent)
}

#[tauri::command]
pub fn pac_status(state: State<'_, Arc<AppState>>) -> R<PacStatus> {
    activation::pac_status(&state).map_err(anyerr)
}

#[tauri::command]
pub async fn pac_reapply(state: State<'_, Arc<AppState>>) -> R<PacStatus> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || activation::apply_system_pac(&st))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    activation::pac_status(&state).map_err(anyerr)
}

#[tauri::command]
pub async fn pac_restore(state: State<'_, Arc<AppState>>) -> R<PacStatus> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || activation::restore_system_pac(&st))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    activation::pac_status(&state).map_err(anyerr)
}

// ---------- CLI ----------

#[tauri::command]
pub fn cli_status() -> R<cli_install::CliStatus> {
    let bin = activation::ensure_pg_binary().map_err(anyerr)?;
    Ok(cli_install::status(&bin))
}

#[tauri::command]
pub async fn cli_install_cmd() -> R<cli_install::CliStatus> {
    let bin = activation::ensure_pg_binary().map_err(anyerr)?;
    let b2 = bin.clone();
    tauri::async_runtime::spawn_blocking(move || cli_install::install(&b2))
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    Ok(cli_install::status(&bin))
}

#[tauri::command]
pub async fn cli_uninstall_cmd() -> R<cli_install::CliStatus> {
    let bin = activation::ensure_pg_binary().map_err(anyerr)?;
    tauri::async_runtime::spawn_blocking(cli_install::uninstall)
        .await
        .map_err(err)?
        .map_err(anyerr)?;
    Ok(cli_install::status(&bin))
}

// ---------- 规则 ----------

#[tauri::command]
pub fn list_rules(state: State<'_, Arc<AppState>>) -> R<Vec<Rule>> {
    state.store.rules().map_err(anyerr)
}

#[tauri::command]
pub fn set_rule_enabled(state: State<'_, Arc<AppState>>, id: String, enabled: bool) -> R<Vec<Rule>> {
    state.store.set_rule_enabled(&id, enabled).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    state.store.rules().map_err(anyerr)
}

#[tauri::command]
pub fn upsert_rule(state: State<'_, Arc<AppState>>, rule: Rule) -> R<Vec<Rule>> {
    state.store.upsert_custom_rule(&rule).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    state.store.rules().map_err(anyerr)
}

#[tauri::command]
pub fn delete_rule(state: State<'_, Arc<AppState>>, id: String) -> R<Vec<Rule>> {
    state.store.delete_custom_rule(&id).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    state.store.rules().map_err(anyerr)
}

#[derive(Serialize)]
pub struct RuleTestResult {
    pub spans: Vec<SpanView>,
    pub redacted: String,
}

#[derive(Serialize)]
pub struct SpanView {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub entity_type: String,
    pub rule_id: String,
}

#[tauri::command]
pub fn test_rules(state: State<'_, Arc<AppState>>, text: String, draft: Option<Rule>) -> R<RuleTestResult> {
    // 测试面板：用库中规则 + 可选的未保存草稿规则，独立的映射表，不污染会话映射
    let mut rules = state.store.rules().map_err(anyerr)?;
    if let Some(d) = draft {
        rules.retain(|r| r.id != d.id);
        rules.push(d);
    }
    let r = redact::Redactor::from_rules(&rules, 1000).map_err(err)?;
    let identity = state.store.active_identity().map_err(anyerr)?;
    let style = state.store.app_settings().map_err(anyerr)?.substitution_style;
    r.configure(&rules, &identity, style).map_err(err)?;
    let spans: Vec<Span> = r.detect(&text);
    let redacted = r.redact_text(&text).text;
    Ok(RuleTestResult {
        spans: spans
            .into_iter()
            .map(|s| SpanView {
                text: text[s.start..s.end].to_string(),
                start: s.start,
                end: s.end,
                entity_type: s.entity_type,
                rule_id: s.rule_id,
            })
            .collect(),
        redacted,
    })
}

// ---------- 隐身身份 ----------

#[derive(Serialize)]
pub struct PersonaView {
    pub personas: Vec<Persona>,
    pub active_persona: Option<String>,
    pub substitution_style: redact::Style,
}

fn persona_view(state: &AppState) -> R<PersonaView> {
    let s = state.store.app_settings().map_err(anyerr)?;
    Ok(PersonaView {
        personas: state.store.personas().map_err(anyerr)?,
        active_persona: s.active_persona,
        substitution_style: s.substitution_style,
    })
}

#[tauri::command]
pub fn list_personas(state: State<'_, Arc<AppState>>) -> R<PersonaView> {
    persona_view(&state)
}

#[tauri::command]
pub fn upsert_persona(state: State<'_, Arc<AppState>>, persona: Persona) -> R<PersonaView> {
    state.store.upsert_persona(&persona).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    persona_view(&state)
}

#[tauri::command]
pub fn delete_persona(state: State<'_, Arc<AppState>>, id: String) -> R<PersonaView> {
    state.store.delete_persona(&id).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    persona_view(&state)
}

/// 切换启用的身份（None 关闭）与替换风格。
#[tauri::command]
pub fn set_persona_options(
    state: State<'_, Arc<AppState>>,
    active_persona: Option<String>,
    substitution_style: redact::Style,
) -> R<PersonaView> {
    if let Some(id) = &active_persona {
        if state.store.persona(id).map_err(anyerr)?.is_none() {
            return Err(format!("身份 {id} 不存在"));
        }
    }
    let mut s = state.store.app_settings().map_err(anyerr)?;
    s.active_persona = active_persona;
    s.substitution_style = substitution_style;
    state.store.save_app_settings(&s).map_err(anyerr)?;
    state.reload_rules().map_err(anyerr)?;
    persona_view(&state)
}

/// 用当前身份 + 风格预览一段文本脱敏后的样子（独立映射表，不污染会话）。
#[tauri::command]
pub fn preview_persona(state: State<'_, Arc<AppState>>, text: String, persona: Option<Persona>) -> R<RuleTestResult> {
    let rules = state.store.rules().map_err(anyerr)?;
    let settings = state.store.app_settings().map_err(anyerr)?;
    let identity = match persona {
        Some(p) => p.fields,
        None => state.store.active_identity().map_err(anyerr)?,
    };
    let r = redact::Redactor::from_rules(&rules, 1000).map_err(err)?;
    r.configure(&rules, &identity, settings.substitution_style).map_err(err)?;
    let spans: Vec<Span> = r.detect(&text);
    let redacted = r.redact_text(&text).text;
    Ok(RuleTestResult {
        spans: spans
            .into_iter()
            .map(|s| SpanView {
                text: text[s.start..s.end].to_string(),
                start: s.start,
                end: s.end,
                entity_type: s.entity_type,
                rule_id: s.rule_id,
            })
            .collect(),
        redacted,
    })
}

#[tauri::command]
pub fn export_rules(state: State<'_, Arc<AppState>>, dest: String) -> R<usize> {
    let custom: Vec<Rule> = state
        .store
        .rules()
        .map_err(anyerr)?
        .into_iter()
        .filter(|r| !r.builtin)
        .collect();
    std::fs::write(&dest, serde_json::to_string_pretty(&custom).map_err(err)?).map_err(err)?;
    Ok(custom.len())
}

#[tauri::command]
pub fn import_rules(state: State<'_, Arc<AppState>>, path: String) -> R<Vec<Rule>> {
    let json = std::fs::read_to_string(&path).map_err(err)?;
    let rules: Vec<Rule> = serde_json::from_str(&json).map_err(err)?;
    for mut r in rules {
        if r.builtin || r.id.starts_with("builtin.") {
            continue;
        }
        r.builtin = false;
        state.store.upsert_custom_rule(&r).map_err(anyerr)?;
    }
    state.reload_rules().map_err(anyerr)?;
    state.store.rules().map_err(anyerr)
}

// ---------- 日志 ----------

#[tauri::command]
pub fn list_requests(state: State<'_, Arc<AppState>>, filter: RequestFilter) -> R<Vec<RequestRow>> {
    state.store.list_requests(&filter).map_err(anyerr)
}

#[tauri::command]
pub fn request_detail(state: State<'_, Arc<AppState>>, id: String) -> R<Option<RequestDetail>> {
    state.store.request_detail(&id).map_err(anyerr)
}

#[derive(Deserialize)]
pub struct StatsQuery {
    pub since_hours: Option<u64>,
}

#[tauri::command]
pub fn stats(state: State<'_, Arc<AppState>>, query: StatsQuery) -> R<Stats> {
    let since = query
        .since_hours
        .map(|h| chrono::Utc::now() - chrono::Duration::hours(h as i64));
    state.store.stats(since).map_err(anyerr)
}

#[derive(Deserialize)]
pub struct UsageQuery {
    /// 窗口长度（小时）
    pub hours: f64,
    /// 时间桶（秒）
    pub bucket_secs: u64,
}

#[tauri::command]
pub fn usage_report(state: State<'_, Arc<AppState>>, query: UsageQuery) -> R<UsageReport> {
    let hours = query.hours.clamp(0.05, 24.0 * 366.0);
    let since = chrono::Utc::now() - chrono::Duration::milliseconds((hours * 3_600_000.0) as i64);
    let prices = state.store.app_settings().map_err(anyerr)?.effective_prices();
    state.store.usage_report(since, query.bucket_secs, &prices).map_err(anyerr)
}

#[derive(Serialize)]
pub struct PricingView {
    pub prices: Vec<ModelPrice>,
    pub customized: bool,
}

#[tauri::command]
pub fn get_pricing(state: State<'_, Arc<AppState>>) -> R<PricingView> {
    let s = state.store.app_settings().map_err(anyerr)?;
    Ok(PricingView {
        customized: !s.pricing.is_empty(),
        prices: s.effective_prices(),
    })
}

/// 保存定价表；传空数组表示恢复内置参考价。
#[tauri::command]
pub fn set_pricing(state: State<'_, Arc<AppState>>, prices: Vec<ModelPrice>) -> R<PricingView> {
    let mut s = state.store.app_settings().map_err(anyerr)?;
    s.pricing = prices
        .into_iter()
        .filter(|p| !p.model_prefix.trim().is_empty())
        .map(|mut p| {
            p.model_prefix = p.model_prefix.trim().to_string();
            p
        })
        .collect();
    state.store.save_app_settings(&s).map_err(anyerr)?;
    Ok(PricingView {
        customized: !s.pricing.is_empty(),
        prices: s.effective_prices(),
    })
}

#[tauri::command]
pub fn clear_logs(state: State<'_, Arc<AppState>>) -> R<()> {
    state.store.clear_logs().map_err(anyerr)
}

#[tauri::command]
pub fn reconcile(state: State<'_, Arc<AppState>>) -> R<Vec<String>> {
    activation::reconcile(&state).map_err(anyerr)
}
