use std::path::PathBuf;

pub const APP_DIR_NAME: &str = "PrivacyGuard";

/// `~/Library/Application Support/PrivacyGuard`
pub fn app_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PG_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| home().join("Library/Application Support"))
        .join(APP_DIR_NAME)
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn ca_dir() -> PathBuf {
    app_data_dir().join("ca")
}

pub fn ca_cert_path() -> PathBuf {
    ca_dir().join(proxy_core::ca::CA_CERT_FILE)
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("privacyguard.sqlite")
}

pub fn backups_dir() -> PathBuf {
    app_data_dir().join("backups")
}

/// 桌面端运行时把监听地址写在这里，供 `pg` 启动器复用。
pub fn runtime_file() -> PathBuf {
    app_data_dir().join("runtime.json")
}

pub fn claude_settings_path() -> PathBuf {
    home().join(".claude/settings.json")
}

pub fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".codex"))
}

pub fn codex_config_path() -> PathBuf {
    codex_home().join("config.toml")
}

pub fn codex_auth_path() -> PathBuf {
    codex_home().join("auth.json")
}
