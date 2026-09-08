//! 设置: kv_settings 读写; 备份/恢复(借助 tauri-plugin-dialog 由前端选择路径, 写入磁盘由 Rust 完成)。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::Db;

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsView {
    pub display_currency: String,
    pub usd_cny_rate: f64,
    /// 透明代理上游地址(如 https://api.deepseek.com)
    pub collector_upstream: String,
}

#[tauri::command]
pub fn get_settings(db: State<'_, Db>) -> Result<SettingsView, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let upstream = read_setting(&conn, "collector_upstream")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com".to_string());
    read_setting(&conn, "display_currency")
        .map(|c| SettingsView {
            display_currency: c.unwrap_or_else(|| "CNY".into()),
            usd_cny_rate: read_setting(&conn, "usd_cny_rate")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(7.1),
            collector_upstream: upstream,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_settings(db: State<'_, Db>, s: SettingsView) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    write_setting(&conn, "display_currency", &s.display_currency).map_err(|e| e.to_string())?;
    write_setting(&conn, "usd_cny_rate", &s.usd_cny_rate.to_string()).map_err(|e| e.to_string())?;
    let upstream = s.collector_upstream.trim().to_string();
    if upstream.is_empty() {
        write_setting(&conn, "collector_upstream", "https://api.deepseek.com")
            .map_err(|e| e.to_string())?;
    } else {
        write_setting(&conn, "collector_upstream", &upstream).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 备份: 将当前数据库复制到 dest_path(由前端 dialog.save 给定)。
#[tauri::command]
pub fn backup_db(db: State<'_, Db>, dest_path: String) -> Result<(), String> {
    // 直接复制 db 文件(未启用 WAL 单独文件更简单; WAL 模式下主文件已含全部提交数据)
    let src = db.path.clone();
    std::fs::copy(&src, &dest_path)
        .map_err(|e| format!("备份失败: {e}"))?;
    Ok(())
}

/// 恢复: 校验源文件后覆盖当前库。Rust 端执行, 返回受影响行数提示前端刷新。
#[tauri::command]
pub fn restore_db(db: State<'_, Db>, src_path: String) -> Result<i64, String> {
    // 校验: 打开的必须是有效的 SQLite 且含 usage_record
    {
        let probe = rusqlite::Connection::open(&src_path).map_err(|e| format!("备份文件无法打开: {e}"))?;
        let has: i64 = probe
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='usage_record'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("备份文件不是有效的 Token Tracker 数据库: {e}"))?;
        if has == 0 {
            return Err("备份文件中不存在 usage_record 表".to_string());
        }
    }
    let src = db.path.clone();
    std::fs::copy(&src_path, &src).map_err(|e| format!("恢复失败: {e}"))?;
    // 记录总数
    let conn = db.lock().map_err(|e| e.to_string())?;
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_record", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(n)
}

fn read_setting(conn: &rusqlite::Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM kv_settings WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

fn write_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO kv_settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}
