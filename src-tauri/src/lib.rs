mod activation;
mod commands;
mod state;
mod tray;

use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, Manager, WindowEvent};

pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PG_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init();

    let state = match AppState::init() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("PrivacyGuard 初始化失败: {e:#}");
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(state.clone())
        .setup(move |app| {
            let handle = app.handle().clone();

            // 请求日志落库后推送到前端
            let h2 = handle.clone();
            state.sink.subscribe(move |log| {
                let _ = h2.emit("request:new", log);
            });

            tray::build(&handle)?;

            // 启动自检 + 按设置启动代理
            let st = state.clone();
            let h3 = handle.clone();
            tauri::async_runtime::spawn(async move {
                match activation::reconcile(&st) {
                    Ok(notes) => {
                        for n in &notes {
                            tracing::info!("{n}");
                        }
                        let _ = h3.emit("app:notes", notes);
                    }
                    Err(e) => tracing::warn!("自检失败: {e:#}"),
                }
                let settings = st.store.app_settings().unwrap_or_default();
                let needs_proxy = settings.start_proxy_on_launch
                    || activation::pac_hosts(&st.store).ok().flatten().is_some()
                    || store::Agent::all()
                        .iter()
                        .any(|a| st.store.activation(*a).map(|x| x.enabled).unwrap_or(false));
                if needs_proxy {
                    if let Err(e) = st.start_proxy().await {
                        tracing::error!("启动代理失败: {e:#}");
                    }
                }
                let _ = tray::refresh(&h3);
                let _ = h3.emit("proxy:status", st.status());

                // 每小时清理一次过期日志
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    let _ = activation::reconcile(&st);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Arc<AppState>>();
                let minimize = state
                    .store
                    .app_settings()
                    .map(|s| s.minimize_to_tray)
                    .unwrap_or(true);
                if minimize {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::proxy_status,
            commands::proxy_start,
            commands::proxy_stop,
            commands::proxy_restart,
            commands::get_settings,
            commands::save_settings,
            commands::app_paths,
            commands::reveal_path,
            commands::ca_view,
            commands::ca_install,
            commands::ca_remove,
            commands::ca_regenerate,
            commands::ca_export,
            commands::agent_view,
            commands::agent_enable,
            commands::agent_disable,
            commands::agent_detect,
            commands::pac_status,
            commands::pac_reapply,
            commands::pac_restore,
            commands::cli_status,
            commands::cli_install_cmd,
            commands::cli_uninstall_cmd,
            commands::list_rules,
            commands::set_rule_enabled,
            commands::upsert_rule,
            commands::delete_rule,
            commands::test_rules,
            commands::list_personas,
            commands::upsert_persona,
            commands::delete_persona,
            commands::set_persona_options,
            commands::preview_persona,
            commands::import_rules,
            commands::export_rules,
            commands::list_requests,
            commands::request_detail,
            commands::stats,
            commands::usage_report,
            commands::get_pricing,
            commands::set_pricing,
            commands::clear_logs,
            commands::reconcile,
        ])
        .run(tauri::generate_context!())
        .expect("运行 PrivacyGuard 失败");
}
