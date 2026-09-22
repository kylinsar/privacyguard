use anyhow::Result;
use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE IF NOT EXISTS settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS rules (
        id TEXT PRIMARY KEY,
        builtin INTEGER NOT NULL DEFAULT 0,
        enabled INTEGER NOT NULL DEFAULT 1,
        json TEXT NOT NULL,
        sort INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS activation_state (
        key TEXT PRIMARY KEY,
        json TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS requests (
        id TEXT PRIMARY KEY,
        started_at TEXT NOT NULL,
        duration_ms INTEGER NOT NULL,
        entry TEXT NOT NULL,
        agent TEXT NOT NULL,
        provider TEXT NOT NULL,
        api TEXT NOT NULL,
        method TEXT NOT NULL,
        host TEXT NOT NULL,
        path TEXT NOT NULL,
        status INTEGER,
        req_bytes INTEGER NOT NULL,
        resp_bytes INTEGER NOT NULL,
        streamed INTEGER NOT NULL,
        redaction_count INTEGER NOT NULL,
        restored_count INTEGER NOT NULL,
        error TEXT,
        warnings TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_requests_started ON requests(started_at DESC);
    CREATE INDEX IF NOT EXISTS idx_requests_agent ON requests(agent);
    CREATE TABLE IF NOT EXISTS model_calls (
        request_id TEXT PRIMARY KEY REFERENCES requests(id) ON DELETE CASCADE,
        provider TEXT NOT NULL,
        api TEXT NOT NULL,
        model TEXT,
        response_model TEXT,
        stream INTEGER NOT NULL,
        message_count INTEGER NOT NULL,
        tool_names TEXT NOT NULL,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        stop_reason TEXT
    );
    CREATE TABLE IF NOT EXISTS redaction_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
        rule_id TEXT NOT NULL,
        entity_type TEXT NOT NULL,
        count INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_redaction_req ON redaction_events(request_id);
    CREATE TABLE IF NOT EXISTS request_bodies (
        request_id TEXT PRIMARY KEY REFERENCES requests(id) ON DELETE CASCADE,
        redacted_body TEXT NOT NULL
    );
    "#,
    // v2: 隐身身份
    r#"
    CREATE TABLE IF NOT EXISTS personas (
        id TEXT PRIMARY KEY,
        json TEXT NOT NULL,
        sort INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL
    );
    "#,
    // v3: 使用监控（首字节延迟、会话、推理 token、模型实际调用的工具）
    r#"
    ALTER TABLE requests ADD COLUMN ttft_ms INTEGER;
    ALTER TABLE requests ADD COLUMN session_id TEXT;
    CREATE INDEX IF NOT EXISTS idx_requests_session ON requests(session_id);
    ALTER TABLE model_calls ADD COLUMN reasoning_tokens INTEGER;
    ALTER TABLE model_calls ADD COLUMN reasoning_effort TEXT;
    ALTER TABLE model_calls ADD COLUMN tool_calls TEXT NOT NULL DEFAULT '[]';
    "#,
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let v = (i + 1) as i64;
        if v > current {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", v)?;
        }
    }
    Ok(())
}
