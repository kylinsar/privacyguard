//! `pg` —— PrivacyGuard 命令行启动器（模式 C）与独立代理。
//!
//! 用法：
//!   pg claude [args…]      通过代理运行 Claude Code
//!   pg codex  [args…]      通过代理运行 Codex
//!   pg run -- <cmd> [args] 通过代理运行任意命令
//!   pg serve [--port N]    前台运行代理
//!   pg ca <print|path|status|install-login|install-system>
//!   pg status
//!
//! 以 `pg-claude` / `pg-codex` 名字调用时等价于 `pg claude` / `pg codex`。

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use platform_macos::{agents, keychain, paths, runtime, shell};
use proxy_core::{CertificateAuthority, LogSink, ProxyConfig};
use redact::Redactor;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use store::{Agent, Store, StoreSink};

#[derive(Parser)]
#[command(name = "pg", version, about = "PrivacyGuard：本地隐私代理启动器")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 通过代理运行 Claude Code
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 通过代理运行 Codex
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 通过代理运行任意命令（注入 HTTPS_PROXY 与 CA 环境变量）
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// 前台运行代理
    Serve {
        #[arg(long)]
        port: Option<u16>,
    },
    /// CA 证书管理
    Ca {
        #[command(subcommand)]
        action: CaAction,
    },
    /// 显示状态
    Status,
}

#[derive(Subcommand)]
enum CaAction {
    /// 打印 PEM
    Print,
    /// 打印证书路径
    Path,
    /// 钥匙串信任状态
    Status,
    /// 安装到登录钥匙串并信任
    InstallLogin,
    /// 安装到系统钥匙串并信任（需管理员）
    InstallSystem,
}

fn main() -> ExitCode {
    init_tracing();
    // pg-claude / pg-codex 别名
    let argv0 = std::env::args().next().unwrap_or_default();
    let base = Path::new(&argv0)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let cli = if base == "pg-claude" || base == "pg-codex" {
        let args: Vec<String> = std::env::args().skip(1).collect();
        Cli {
            cmd: if base == "pg-claude" {
                Cmd::Claude { args }
            } else {
                Cmd::Codex { args }
            },
        }
    } else {
        Cli::parse()
    };

    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pg: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    let filter = std::env::var("PG_LOG").unwrap_or_else(|_| "warn,pg::request=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Claude { args } => launch(Some(Agent::Claude), "claude", args),
        Cmd::Codex { args } => launch(Some(Agent::Codex), "codex", args),
        Cmd::Run { args } => {
            let (cmd, rest) = args.split_first().ok_or_else(|| anyhow!("缺少命令"))?;
            launch(None, cmd, rest.to_vec())
        }
        Cmd::Serve { port } => serve(port),
        Cmd::Ca { action } => ca(action),
        Cmd::Status => status(),
    }
}

// ---------------- 共享环境 ----------------

struct Env {
    ca: Arc<CertificateAuthority>,
    store: Arc<Store>,
}

fn open_env() -> Result<Env> {
    let ca = Arc::new(CertificateAuthority::load_or_create(&paths::ca_dir())?);
    let store = Arc::new(Store::open(&paths::db_path())?);
    Ok(Env { ca, store })
}

fn build_redactor(store: &Store, cfg: &ProxyConfig) -> Result<Arc<Redactor>> {
    Ok(Arc::new(store.build_redactor(cfg.placeholder_capacity)?))
}

// ---------------- 启动器（模式 C） ----------------

fn launch(agent: Option<Agent>, program: &str, args: Vec<String>) -> Result<ExitCode> {
    let env = open_env()?;

    // 复用桌面端代理，否则内嵌启动
    let rt = tokio::runtime::Runtime::new()?;
    let (proxy_url, _guard) = match runtime::read_live() {
        Some(info) => {
            tracing::info!("复用桌面端代理 {}", info.listen);
            (format!("http://{}", info.listen), None)
        }
        None => {
            let mut cfg = env.store.app_settings()?.to_proxy_config();
            cfg.listen.set_port(0);
            let redactor = build_redactor(&env.store, &cfg)?;
            let sink: Arc<dyn LogSink> = StoreSink::new(env.store.clone());
            let (_proxy, handle) = rt.block_on(proxy_core::start(cfg, env.ca.clone(), redactor, sink))?;
            let url = format!("http://{}", handle.local_addr);
            tracing::info!("已启动内嵌代理 {url}");
            (url, Some(handle))
        }
    };

    let bin_env = agent.map(|a| format!("PG_{}_BIN", a.key().to_ascii_uppercase()));
    let binary = bin_env
        .as_deref()
        .and_then(|k| std::env::var(k).ok())
        .filter(|s| !s.is_empty())
        .or_else(|| shell::which_via_login_shell(program))
        .ok_or_else(|| {
            anyhow!(
                "找不到 `{program}`。请确认已安装，或用 {} 指定绝对路径",
                bin_env.unwrap_or_else(|| "PATH".into())
            )
        })?;

    let mut cmd = Command::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let ca_path = env.ca.cert_path().to_path_buf();
    let inject = match agent {
        Some(a) => agents::launcher_env(a, &proxy_url, &ca_path),
        None => {
            let mut v = agents::launcher_env(Agent::Claude, &proxy_url, &ca_path);
            v.push(("CODEX_CA_CERTIFICATE".into(), ca_path.to_string_lossy().to_string()));
            v.push(("SSL_CERT_FILE".into(), ca_path.to_string_lossy().to_string()));
            v
        }
    };
    for (k, v) in inject {
        cmd.env(k, v);
    }
    eprintln!("PrivacyGuard: 通过 {proxy_url} 保护 {program}");

    let mut child = cmd.spawn().with_context(|| format!("启动 {binary}"))?;
    let status = child.wait()?;

    if let Some(h) = _guard {
        h.shutdown();
    }
    // 给异步落库线程一点时间
    std::thread::sleep(std::time::Duration::from_millis(150));
    drop(rt);
    Ok(ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8))
}

// ---------------- 前台代理 ----------------

fn serve(port: Option<u16>) -> Result<ExitCode> {
    let env = open_env()?;
    let mut cfg = env.store.app_settings()?.to_proxy_config();
    if let Some(p) = port {
        cfg.listen.set_port(p);
    }
    let redactor = build_redactor(&env.store, &cfg)?;
    let sink: Arc<dyn LogSink> = StoreSink::new(env.store.clone());
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let (_proxy, handle) = proxy_core::start(cfg, env.ca.clone(), redactor, sink).await?;
        runtime::write(&runtime::RuntimeInfo {
            pid: std::process::id(),
            listen: handle.local_addr,
            started_at: chrono::Utc::now().to_rfc3339(),
            ca_fingerprint: env.ca.fingerprint_sha256(),
        })?;
        eprintln!("PrivacyGuard 代理运行中: http://{}", handle.local_addr);
        eprintln!("  PAC:  http://{}/proxy.pac", handle.local_addr);
        eprintln!("  CA:   {}", env.ca.cert_path().display());
        eprintln!("Ctrl-C 退出");
        tokio::signal::ctrl_c().await?;
        handle.shutdown();
        runtime::clear();
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(ExitCode::SUCCESS)
}

// ---------------- CA ----------------

fn ca(action: CaAction) -> Result<ExitCode> {
    let ca = CertificateAuthority::load_or_create(&paths::ca_dir())?;
    match action {
        CaAction::Print => print!("{}", ca.cert_pem()),
        CaAction::Path => println!("{}", ca.cert_path().display()),
        CaAction::Status => {
            let st = keychain::status(ca.cert_path(), ca.cert_der().as_ref());
            println!("{}", serde_json::to_string_pretty(&st)?);
        }
        CaAction::InstallLogin => {
            keychain::install(keychain::KeychainKind::Login, ca.cert_path())?;
            println!("已安装到登录钥匙串并设为信任");
        }
        CaAction::InstallSystem => {
            keychain::install(keychain::KeychainKind::System, ca.cert_path())?;
            println!("已安装到系统钥匙串并设为信任");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn status() -> Result<ExitCode> {
    let ca = CertificateAuthority::load_or_create(&paths::ca_dir())?;
    println!("数据目录: {}", paths::app_data_dir().display());
    println!("CA 指纹 (SHA-256): {}", ca.fingerprint_sha256());
    match runtime::read_live() {
        Some(i) => println!("桌面端代理: 运行中 http://{} (pid {})", i.listen, i.pid),
        None => println!("桌面端代理: 未运行（pg 将内嵌启动临时代理）"),
    }
    for a in Agent::all() {
        let info = agents::detect(a);
        println!(
            "{}: {} {}",
            a.key(),
            if info.installed { "已安装" } else { "未安装" },
            info.version.unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}
