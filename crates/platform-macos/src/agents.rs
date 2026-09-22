//! 编码代理（Claude Code / Codex）的检测，以及模式 A 的配置写入 / 回滚。

use crate::paths;
use crate::shell::{run_lenient, which_via_login_shell};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use store::Agent;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentInfo {
    pub agent: Agent,
    pub installed: bool,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub config_path: String,
    pub config_exists: bool,
    /// 配置中已经存在我们写入的键
    pub config_managed_by_us: bool,
    /// 配置中存在与我们冲突的用户自有设置（如已有 HTTPS_PROXY）
    pub conflicts: Vec<String>,
    /// Codex 登录方式：apikey / chatgpt / unknown
    pub auth_mode: Option<String>,
}

pub fn detect(agent: Agent) -> AgentInfo {
    let name = agent.key();
    let binary = which_via_login_shell(name);
    let version = binary.as_ref().and_then(|b| {
        let (ok, out) = run_lenient(b, &["--version"]);
        ok.then(|| out.lines().next().unwrap_or("").trim().to_string())
    });
    let config_path = match agent {
        Agent::Claude => paths::claude_settings_path(),
        Agent::Codex => paths::codex_config_path(),
    };
    let mut info = AgentInfo {
        agent,
        installed: binary.is_some(),
        binary,
        version,
        config_path: config_path.to_string_lossy().to_string(),
        config_exists: config_path.exists(),
        ..Default::default()
    };
    match agent {
        Agent::Claude => {
            if let Ok(v) = read_json(&config_path) {
                let env = v.get("env").and_then(|e| e.as_object());
                if let Some(env) = env {
                    let managed = env.get(MANAGED_MARK_KEY).is_some();
                    info.config_managed_by_us = managed;
                    if !managed {
                        for k in ["HTTPS_PROXY", "HTTP_PROXY", "ANTHROPIC_BASE_URL"] {
                            if env.contains_key(k) {
                                info.conflicts.push(format!("env.{k} 已由用户设置"));
                            }
                        }
                    }
                }
            }
        }
        Agent::Codex => {
            info.auth_mode = Some(codex_auth_mode());
            if let Ok(s) = fs::read_to_string(&config_path) {
                info.config_managed_by_us = s.contains(MANAGED_MARK_COMMENT);
                if !info.config_managed_by_us {
                    if let Ok(doc) = s.parse::<toml_edit::DocumentMut>() {
                        for k in ["openai_base_url", "chatgpt_base_url"] {
                            if doc.get(k).is_some() {
                                info.conflicts.push(format!("{k} 已由用户设置"));
                            }
                        }
                    }
                }
            }
        }
    }
    info
}

/// 读取 `~/.codex/auth.json` 判定登录方式。
pub fn codex_auth_mode() -> String {
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.is_empty()) {
        return "apikey".into();
    }
    match read_json(&paths::codex_auth_path()) {
        Ok(v) => {
            if let Some(m) = v.get("auth_mode").and_then(|m| m.as_str()) {
                return m.to_ascii_lowercase();
            }
            if v.get("OPENAI_API_KEY").and_then(|k| k.as_str()).is_some_and(|k| !k.is_empty()) {
                return "apikey".into();
            }
            if v.get("tokens").is_some() {
                return "chatgpt".into();
            }
            "unknown".into()
        }
        Err(_) => "unknown".into(),
    }
}

// ---------------- 模式 A：写配置 ----------------

const MANAGED_MARK_KEY: &str = "PG_MANAGED";
const MANAGED_MARK_COMMENT: &str = "# managed-by: PrivacyGuard";

/// 回滚信息（存到 activation_state.rollback）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigRollback {
    pub backup_path: Option<String>,
    /// 写入前配置文件是否存在
    pub existed: bool,
    /// 我们覆盖前的原值（key -> 原值 JSON；None 表示原本不存在）
    pub previous: Map<String, Value>,
}

fn read_json(path: &Path) -> Result<Value> {
    let s = fs::read_to_string(path)?;
    if s.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(&s)?)
}

fn backup_file(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let dir = paths::backups_dir();
    fs::create_dir_all(&dir)?;
    let name = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let dst = dir.join(name);
    fs::copy(path, &dst)?;
    Ok(Some(dst))
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("pg-tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Claude Code：在 `~/.claude/settings.json` 的 `env` 中写入代理与 CA。
pub fn claude_enable(proxy_url: &str, ca_pem_path: &Path) -> Result<ConfigRollback> {
    let path = paths::claude_settings_path();
    let existed = path.exists();
    let mut root = if existed {
        read_json(&path).with_context(|| format!("解析 {}", path.display()))?
    } else {
        Value::Object(Map::new())
    };
    let backup = backup_file(&path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.json 顶层不是对象"))?;
    if !obj.get("env").is_some_and(|e| e.is_object()) {
        obj.insert("env".into(), Value::Object(Map::new()));
    }
    let env = obj.get_mut("env").unwrap().as_object_mut().unwrap();

    let mut previous = Map::new();
    let writes = [
        ("HTTPS_PROXY", Value::String(proxy_url.to_string())),
        ("HTTP_PROXY", Value::String(proxy_url.to_string())),
        ("NO_PROXY", Value::String("localhost,127.0.0.1".into())),
        (
            "NODE_EXTRA_CA_CERTS",
            Value::String(ca_pem_path.to_string_lossy().to_string()),
        ),
        (MANAGED_MARK_KEY, Value::String("1".into())),
    ];
    for (k, v) in writes {
        previous.insert(k.to_string(), env.get(k).cloned().unwrap_or(Value::Null));
        env.insert(k.to_string(), v);
    }
    write_atomic(&path, &serde_json::to_string_pretty(&root)?)?;
    Ok(ConfigRollback {
        backup_path: backup.map(|p| p.to_string_lossy().to_string()),
        existed,
        previous,
    })
}

pub fn claude_disable(rb: &ConfigRollback) -> Result<()> {
    let path = paths::claude_settings_path();
    if !path.exists() {
        return Ok(());
    }
    let mut root = read_json(&path)?;
    if let Some(env) = root.get_mut("env").and_then(|e| e.as_object_mut()) {
        for (k, prev) in &rb.previous {
            match prev {
                Value::Null => {
                    env.remove(k);
                }
                v => {
                    env.insert(k.clone(), v.clone());
                }
            }
        }
        env.remove(MANAGED_MARK_KEY);
        if env.is_empty() {
            root.as_object_mut().unwrap().remove("env");
        }
    }
    if !rb.existed && root.as_object().is_some_and(|o| o.is_empty()) {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    write_atomic(&path, &serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

/// Codex：在 `~/.codex/config.toml` 写入 base URL 覆盖，指向本地代理的反向入口。
///
/// - ChatGPT 登录：只写 `chatgpt_base_url`。`openai_base_url` 会让 Codex 把 ChatGPT 的 OAuth
///   token 发到 api.openai.com，得到 `401 Missing scopes: api.responses.write`。
/// - API Key 登录：写 `openai_base_url`；`chatgpt_base_url` 同时写上以覆盖插件等后端调用。
pub fn codex_enable(proxy_base: &str) -> Result<ConfigRollback> {
    codex_enable_with_auth(proxy_base, &codex_auth_mode())
}

pub fn codex_enable_with_auth(proxy_base: &str, auth_mode: &str) -> Result<ConfigRollback> {
    let path = paths::codex_config_path();
    let existed = path.exists();
    let text = if existed {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let backup = backup_file(&path)?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("解析 {}", path.display()))?;

    let base = proxy_base.trim_end_matches('/');
    let mut writes = vec![("chatgpt_base_url", format!("{base}/chatgpt/backend-api/"))];
    let mut previous = Map::new();
    if auth_mode == "apikey" {
        writes.push(("openai_base_url", format!("{base}/openai/v1")));
    } else if let Some(prev) = doc.get("openai_base_url").and_then(|i| i.as_str()) {
        // 用户此前手动写过 openai_base_url：ChatGPT 登录下它会导致 401，先移除并记入回滚
        previous.insert("openai_base_url".into(), Value::String(prev.to_string()));
        doc.remove("openai_base_url");
    }
    for (k, v) in writes {
        let prev = doc
            .get(k)
            .and_then(|i| i.as_str())
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null);
        previous.insert(k.to_string(), prev);
        let mut item = toml_edit::value(v);
        if let Some(val) = item.as_value_mut() {
            val.decor_mut().set_suffix(format!(" {MANAGED_MARK_COMMENT}"));
        }
        doc[k] = item;
    }
    write_atomic(&path, &doc.to_string())?;
    Ok(ConfigRollback {
        backup_path: backup.map(|p| p.to_string_lossy().to_string()),
        existed,
        previous,
    })
}

pub fn codex_disable(rb: &ConfigRollback) -> Result<()> {
    let path = paths::codex_config_path();
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    for (k, prev) in &rb.previous {
        match prev {
            Value::String(s) => {
                doc[k] = toml_edit::value(s.clone());
            }
            _ => {
                doc.remove(k);
            }
        }
    }
    let out = doc.to_string();
    if !rb.existed && out.trim().is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    write_atomic(&path, &out)?;
    Ok(())
}

/// 模式 C 启动器需要注入的环境变量。
pub fn launcher_env(agent: Agent, proxy_url: &str, ca_pem_path: &Path) -> Vec<(String, String)> {
    let ca = ca_pem_path.to_string_lossy().to_string();
    let mut env = vec![
        ("HTTPS_PROXY".to_string(), proxy_url.to_string()),
        ("HTTP_PROXY".to_string(), proxy_url.to_string()),
        ("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string()),
        ("PG_ACTIVE".to_string(), "1".to_string()),
    ];
    match agent {
        Agent::Claude => env.push(("NODE_EXTRA_CA_CERTS".into(), ca)),
        Agent::Codex => env.push(("CODEX_CA_CERTIFICATE".into(), ca)),
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_toml_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pg-codex-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CODEX_HOME", &dir);
        std::env::set_var("PG_DATA_DIR", dir.join("data"));
        let cfg = dir.join("config.toml");
        fs::write(&cfg, "# user comment\nmodel = \"gpt-5\"\n\n[sandbox]\nmode = \"read-only\"\n").unwrap();

        // API Key 登录：两个 base url 都写
        let rb = codex_enable_with_auth("http://127.0.0.1:7890", "apikey").unwrap();
        let after = fs::read_to_string(&cfg).unwrap();
        assert!(after.contains("openai_base_url = \"http://127.0.0.1:7890/openai/v1\""));
        assert!(after.contains("chatgpt_base_url = \"http://127.0.0.1:7890/chatgpt/backend-api/\""));
        assert!(after.contains("# user comment"));
        assert!(after.contains("[sandbox]"));

        codex_disable(&rb).unwrap();
        let restored = fs::read_to_string(&cfg).unwrap();
        assert!(!restored.contains("openai_base_url"));
        assert!(!restored.contains("chatgpt_base_url"));
        assert!(restored.contains("model = \"gpt-5\""));

        // ChatGPT 登录：不写 openai_base_url；用户遗留的 openai_base_url 被移除并可回滚
        fs::write(&cfg, "model = \"gpt-5\"\nopenai_base_url = \"https://gw.example/v1\"\n").unwrap();
        let rb = codex_enable_with_auth("http://127.0.0.1:7890", "chatgpt").unwrap();
        let after = fs::read_to_string(&cfg).unwrap();
        assert!(!after.contains("openai_base_url"), "{after}");
        assert!(after.contains("chatgpt_base_url = \"http://127.0.0.1:7890/chatgpt/backend-api/\""));
        codex_disable(&rb).unwrap();
        let restored = fs::read_to_string(&cfg).unwrap();
        assert!(restored.contains("openai_base_url = \"https://gw.example/v1\""), "{restored}");
        assert!(!restored.contains("chatgpt_base_url"));
        let _ = fs::remove_dir_all(dir);
    }
}
