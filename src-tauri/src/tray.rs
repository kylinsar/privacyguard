//! 托盘图标与菜单。

use crate::state::AppState;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

pub const TRAY_ID: &str = "pg-tray";

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<TrayIcon<R>> {
    let menu = build_menu(app, false)?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().expect("默认图标"))
        .icon_as_template(false)
        .tooltip("PrivacyGuard")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_string();
            let app = app.clone();
            match id.as_str() {
                "show" => show_main(&app),
                "toggle" => {
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<Arc<AppState>>();
                        if state.is_running() {
                            state.stop_proxy().await;
                        } else if let Err(e) = state.start_proxy().await {
                            tracing::error!("托盘启动代理失败: {e:#}");
                        }
                        let _ = refresh(&app);
                    });
                }
                "quit" => {
                    let state = app.state::<Arc<AppState>>();
                    let s = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        s.stop_proxy().await;
                    });
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(tray)
}

fn build_menu<R: Runtime>(app: &AppHandle<R>, running: bool) -> tauri::Result<Menu<R>> {
    let status = MenuItem::with_id(
        app,
        "status",
        if running { "代理：运行中" } else { "代理：已停止" },
        false,
        None::<&str>,
    )?;
    let toggle = MenuItem::with_id(
        app,
        "toggle",
        if running { "停止代理" } else { "启动代理" },
        true,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, "show", "打开 PrivacyGuard", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, Some("CmdOrCtrl+Q"))?;
    Menu::with_items(
        app,
        &[
            &status,
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &show,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

pub fn refresh<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let running = app.state::<Arc<AppState>>().is_running();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(build_menu(app, running)?))?;
        tray.set_tooltip(Some(if running {
            "PrivacyGuard · 代理运行中"
        } else {
            "PrivacyGuard · 代理已停止"
        }))?;
    }
    Ok(())
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
