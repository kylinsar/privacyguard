//! 应用全局状态：存储、CA、脱敏器、代理生命周期。

use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use platform_macos::{paths, runtime};
use proxy_core::{CertificateAuthority, LogSink, Proxy, ProxyHandle};
use redact::Redactor;
use serde::Serialize;
use std::sync::Arc;
use store::{Store, StoreSink};

pub struct Running {
    pub proxy: Arc<Proxy>,
    pub handle: ProxyHandle,
}

pub struct AppState {
    pub store: Arc<Store>,
    pub ca: RwLock<Arc<CertificateAuthority>>,
    pub redactor: Arc<Redactor>,
    pub sink: Arc<StoreSink>,
    pub proxy: Mutex<Option<Running>>,
    pub last_error: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub listen: Option<String>,
    pub proxy_url: Option<String>,
    pub pac_url: Option<String>,
    pub intercept_hosts: Vec<String>,
    pub last_error: Option<String>,
    /// 当前在途（尚未完成）的请求数
    pub inflight: usize,
    /// 本次代理启动以来处理的请求数
    pub total_requests: u64,
}

impl AppState {
    pub fn init() -> Result<Self> {
        let store = Arc::new(Store::open(&paths::db_path()).context("打开数据库")?);
        let ca = Arc::new(CertificateAuthority::load_or_create(&paths::ca_dir()).context("加载 CA")?);
        let settings = store.app_settings()?;
        let cfg = settings.to_proxy_config();
        let redactor = Arc::new(store.build_redactor(cfg.placeholder_capacity).context("编译规则")?);
        let sink = StoreSink::new(store.clone());
        Ok(Self {
            store,
            ca: RwLock::new(ca),
            redactor,
            sink,
            proxy: Mutex::new(None),
            last_error: Mutex::new(None),
        })
    }

    pub fn ca(&self) -> Arc<CertificateAuthority> {
        self.ca.read().clone()
    }

    pub fn is_running(&self) -> bool {
        self.proxy.lock().as_ref().is_some_and(|r| r.handle.is_running())
    }

    pub fn proxy_url(&self) -> Result<String> {
        let port = self.store.app_settings()?.port;
        Ok(format!("http://127.0.0.1:{port}"))
    }

    pub fn pac_url(&self) -> Result<String> {
        Ok(format!("{}/proxy.pac", self.proxy_url()?))
    }

    pub fn status(&self) -> ProxyStatus {
        let settings = self.store.app_settings().unwrap_or_default();
        let guard = self.proxy.lock();
        let running = guard.as_ref().is_some_and(|r| r.handle.is_running());
        let listen = guard.as_ref().map(|r| r.handle.local_addr.to_string());
        let hosts = guard
            .as_ref()
            .map(|r| r.proxy.config().intercept_hosts)
            .unwrap_or(settings.intercept_hosts.clone());
        let (inflight, total_requests) = guard
            .as_ref()
            .map(|r| {
                let m = r.proxy.metrics();
                (
                    m.inflight.load(std::sync::atomic::Ordering::Relaxed),
                    m.total_requests.load(std::sync::atomic::Ordering::Relaxed),
                )
            })
            .unwrap_or((0, 0));
        ProxyStatus {
            running,
            inflight,
            total_requests,
            listen: listen.clone(),
            proxy_url: listen.as_ref().map(|l| format!("http://{l}")),
            pac_url: listen.as_ref().map(|l| format!("http://{l}/proxy.pac")),
            intercept_hosts: hosts,
            last_error: self.last_error.lock().clone(),
        }
    }

    pub async fn start_proxy(&self) -> Result<ProxyStatus> {
        if self.is_running() {
            return Ok(self.status());
        }
        let settings = self.store.app_settings()?;
        let mut cfg = settings.to_proxy_config();
        cfg.pac_hosts = crate::activation::pac_hosts(&self.store)?;
        // 重新加载规则 / 身份 / 风格，保证与库一致
        self.store.configure_redactor(&self.redactor)?;
        let sink: Arc<dyn LogSink> = self.sink.clone();
        match proxy_core::start(cfg, self.ca(), self.redactor.clone(), sink).await {
            Ok((proxy, handle)) => {
                let _ = runtime::write(&runtime::RuntimeInfo {
                    pid: std::process::id(),
                    listen: handle.local_addr,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    ca_fingerprint: self.ca().fingerprint_sha256(),
                });
                *self.proxy.lock() = Some(Running { proxy, handle });
                *self.last_error.lock() = None;
            }
            Err(e) => {
                *self.last_error.lock() = Some(format!("{e:#}"));
                return Err(e);
            }
        }
        Ok(self.status())
    }

    pub async fn stop_proxy(&self) -> ProxyStatus {
        let running = self.proxy.lock().take();
        if let Some(r) = running {
            r.handle.shutdown();
            r.handle.wait().await;
        }
        runtime::clear();
        self.status()
    }

    pub async fn restart_proxy(&self) -> Result<ProxyStatus> {
        self.stop_proxy().await;
        self.start_proxy().await
    }

    /// 不重启地把最新设置推给运行中的代理（拦截域名、调试开关、PAC 域名）。
    pub fn push_config(&self) -> Result<()> {
        let settings = self.store.app_settings()?;
        let guard = self.proxy.lock();
        if let Some(r) = guard.as_ref() {
            let mut cfg = r.proxy.config();
            cfg.intercept_hosts = settings.intercept_hosts.clone();
            cfg.store_redacted_bodies = settings.store_redacted_bodies;
            cfg.restore_responses = settings.restore_responses;
            cfg.pac_hosts = crate::activation::pac_hosts(&self.store)?;
            r.proxy.update_config(cfg);
        }
        Ok(())
    }

    /// 重新加载规则、隐身身份与替换风格（任一项在 UI 中修改后调用）。
    pub fn reload_rules(&self) -> Result<()> {
        self.store.configure_redactor(&self.redactor)?;
        Ok(())
    }

    /// 重新生成 CA：需要停代理、替换、再启动。
    pub async fn regenerate_ca(&self) -> Result<()> {
        let was_running = self.is_running();
        self.stop_proxy().await;
        let ca = Arc::new(CertificateAuthority::regenerate(&paths::ca_dir())?);
        *self.ca.write() = ca;
        if was_running {
            self.start_proxy().await?;
        }
        Ok(())
    }
}

/// 与 App 同目录的 `pg` 启动器（Tauri sidecar 放在 Contents/MacOS/）。
pub fn pg_binary_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let p = dir.join("pg");
    if p.exists() {
        return Some(p);
    }
    // 开发模式：cargo target 目录
    let alt = dir.join("pg-aarch64-apple-darwin");
    alt.exists().then_some(alt)
}
