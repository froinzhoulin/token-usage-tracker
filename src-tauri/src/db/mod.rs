use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::domain::price;

/// 应用持有的 SQLite 连接（单写者多读者由调用方保证）。
/// 内部 Arc 包装, 供 collector 后台线程共享同一连接。
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    pub path: PathBuf,
}

impl Db {
    pub fn new(conn: Connection, path: PathBuf) -> Self {
        Self { conn: Arc::new(Mutex::new(conn)), path }
    }

    pub fn lock(&self) -> std::sync::LockResult<MutexGuard<'_, Connection>> {
        self.conn.lock()
    }

    /// 返回共享连接句柄(供后台线程使用)。
    pub fn shared(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}

/// 打开（或创建）数据库并设置连接级 PRAGMA。
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    Ok(conn)
}

/// 当前 schema 版本。每次结构变更 +1，并在 migrate 中追加增量脚本。
const SCHEMA_VERSION: u32 = 3;

/// 执行增量迁移并 seed 内置价格库。返回迁移后的版本号。
pub fn migrate(conn: &Connection) -> rusqlite::Result<u32> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current < 1 {
        conn.execute_batch(
            r#"
            -- 键值设置(展示币种/汇率等)
            CREATE TABLE IF NOT EXISTS kv_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
    }

    if current < 2 {
        conn.execute_batch(
            r#"
            -- 厂商
            CREATE TABLE IF NOT EXISTS provider (
                id   INTEGER PRIMARY KEY,
                code TEXT UNIQUE NOT NULL,
                name TEXT NOT NULL
            );

            -- 模型
            CREATE TABLE IF NOT EXISTS model (
                id          INTEGER PRIMARY KEY,
                provider_id INTEGER NOT NULL REFERENCES provider(id),
                name        TEXT NOT NULL,
                builtin     INTEGER NOT NULL DEFAULT 1,
                UNIQUE(provider_id, name)
            );

            -- 单价: 以 currency(默认USD/1M tokens) 计价;
            -- 同一模型允许 builtin 与 custom 两行, custom 优先
            CREATE TABLE IF NOT EXISTS price (
                id                 INTEGER PRIMARY KEY,
                model_id           INTEGER NOT NULL REFERENCES model(id),
                currency           TEXT NOT NULL DEFAULT 'USD',
                input_per_mtok     REAL NOT NULL,
                output_per_mtok    REAL NOT NULL,
                cached_input_per_mtok REAL,
                source             TEXT NOT NULL DEFAULT 'builtin',  -- builtin|custom
                updated_at         TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(model_id, currency, source)
            );

            -- 导入批次 (先于 usage_record 建立, 供其外键引用)
            CREATE TABLE IF NOT EXISTS import_batch (
                id           INTEGER PRIMARY KEY,
                file_name    TEXT,
                imported_at  TEXT NOT NULL DEFAULT (datetime('now')),
                total_rows   INTEGER NOT NULL DEFAULT 0,
                ok_rows      INTEGER NOT NULL DEFAULT 0,
                failed_rows  INTEGER NOT NULL DEFAULT 0,
                mapping_json TEXT
            );

            -- 用量明细(核心)
            CREATE TABLE IF NOT EXISTS usage_record (
                id              INTEGER PRIMARY KEY,
                recorded_at     TEXT NOT NULL,               -- ISO8601 UTC
                source          TEXT NOT NULL,               -- import_csv|import_json|manual|live
                batch_id        INTEGER REFERENCES import_batch(id),
                provider_code   TEXT,
                model_name      TEXT,
                session_id      TEXT,
                request_id      TEXT,
                prompt_tokens   INTEGER,
                completion_tokens INTEGER,
                cached_tokens   INTEGER,
                total_tokens    INTEGER,
                cost_usd        REAL,                        -- 统一换算为 USD
                cost_currency   TEXT NOT NULL DEFAULT 'USD', -- 原始/换算币种标注
                cost_source     TEXT,                        -- official|computed|manual
                project         TEXT,
                tags            TEXT,                        -- JSON 数组字符串
                note            TEXT,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- 去重键: 导入时基于 request_id / (recorded_at+model+token 指纹)
            CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_dedup
                ON usage_record(request_id) WHERE request_id IS NOT NULL AND request_id <> '';
            CREATE INDEX IF NOT EXISTS idx_usage_time   ON usage_record(recorded_at);
            CREATE INDEX IF NOT EXISTS idx_usage_model  ON usage_record(provider_code, model_name);
            CREATE INDEX IF NOT EXISTS idx_usage_project ON usage_record(project);

            -- 预算 (M2 使用, 先建结构)
            CREATE TABLE IF NOT EXISTS budget (
                id             INTEGER PRIMARY KEY,
                name           TEXT NOT NULL,
                scope          TEXT NOT NULL DEFAULT 'global', -- global|project|tag|provider
                scope_value    TEXT,
                period         TEXT NOT NULL DEFAULT 'month',   -- day|month|custom
                period_start   TEXT,
                period_end     TEXT,
                limit_type     TEXT NOT NULL,                   -- tokens|cost_usd
                limit_value    REAL NOT NULL,
                thresholds_json TEXT,
                active         INTEGER NOT NULL DEFAULT 1
            );
            "#,
        )?;
    }

    if current < 3 {
        // 修正 Claude Code 检测器的历史写入错误:
        // 早期版本把模型名同时写进 provider_code, 导致明细渲染成
        // "MiniMax-M3 MiniMax-M3"(厂商与模型重复), 并把模型名当成一家厂商计数。
        // 只清理 claude_code 来源中 "provider_code 与 model_name 完全相同" 的行,
        // 不触碰其它来源的数据。
        conn.execute(
            "UPDATE usage_record
                SET provider_code = NULL
              WHERE source = 'claude_code'
                AND provider_code IS NOT NULL
                AND provider_code = model_name",
            [],
        )?;
    }

    // seed 内置厂商/模型/价格(幂等: 仅当 provider 表为空)
    price::seed_builtin(conn)?;

    // 默认设置
    seed_settings(conn)?;

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(SCHEMA_VERSION)
}

/// 首次启动写入默认设置(幂等)。
fn seed_settings(conn: &Connection) -> rusqlite::Result<()> {
    let defaults: [(&str, &str); 4] = [
        ("display_currency", "CNY"),
        ("usd_cny_rate", "7.1"),
        ("collector_port", "8765"),
        ("collector_upstream", "https://api.deepseek.com"),
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO kv_settings(key, value) VALUES(?1, ?2)",
            rusqlite::params![k, v],
        )?;
    }
    Ok(())
}

/// 返回当前 user_version（只读，供诊断）。
pub fn version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v3 迁移: 清空 claude_code 中 provider_code == model_name 的脏数据,
    /// 且不误伤其它来源(它们的厂商与模型本来就不同)。
    #[test]
    fn migration_v3_cleans_claude_provider_dup() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();

        // 模拟旧版本写入的脏数据
        conn.execute(
            "INSERT INTO usage_record(recorded_at, source, provider_code, model_name)
             VALUES('2026-09-10T00:00:00.000Z','claude_code','MiniMax-M3','MiniMax-M3')",
            [],
        )
        .unwrap();
        // 正常数据(厂商与模型不同) —— 不应被改动
        conn.execute(
            "INSERT INTO usage_record(recorded_at, source, provider_code, model_name)
             VALUES('2026-09-10T00:00:00.000Z','dsh','deepseek','deepseek-v4-flash')",
            [],
        )
        .unwrap();

        // 回退版本号, 触发 v3 迁移重跑
        conn.pragma_update(None, "user_version", 2).unwrap();
        migrate(&conn).unwrap();

        let bad: Option<String> = conn
            .query_row(
                "SELECT provider_code FROM usage_record WHERE source='claude_code'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad, None, "重复的厂商字段应被清空");

        let good: Option<String> = conn
            .query_row(
                "SELECT provider_code FROM usage_record WHERE source='dsh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(good.as_deref(), Some("deepseek"), "其它来源不应被误伤");
    }
}
