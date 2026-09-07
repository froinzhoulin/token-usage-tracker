use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// 应用持有的 SQLite 连接（单写者多读者由调用方保证）。
pub struct Db {
    conn: Mutex<Connection>,
    pub path: PathBuf,
}

impl Db {
    pub fn new(conn: Connection, path: PathBuf) -> Self {
        Self { conn: Mutex::new(conn), path }
    }

    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex poisoned")
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
const SCHEMA_VERSION: u32 = 1;

/// 执行增量迁移。返回迁移后的版本号。
pub fn migrate(conn: &Connection) -> rusqlite::Result<u32> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current < 1 {
        conn.execute_batch(
            r#"
            -- 键值设置(币种/汇率/默认区间等)
            CREATE TABLE IF NOT EXISTS kv_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(SCHEMA_VERSION)
}

/// 返回当前 user_version（只读，供诊断）。
pub fn version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}
