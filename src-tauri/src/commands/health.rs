use serde::Serialize;
use tauri::State;

use crate::db::{self, Db};

#[derive(Serialize)]
pub struct HealthInfo {
    app_version: String,
    db_ready: bool,
    db_path: Option<String>,
    db_version: u32,
    message: String,
}

#[tauri::command]
pub fn health(db: State<'_, Db>) -> HealthInfo {
    let path = db.path.display().to_string();
    match db.lock() {
        Ok(conn) => match db::version(&conn) {
            Ok(v) => HealthInfo {
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                db_ready: true,
                db_path: Some(path),
                db_version: v,
                message: "Backend OK, SQLite ready".to_string(),
            },
            Err(e) => HealthInfo {
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                db_ready: false,
                db_path: Some(path),
                db_version: 0,
                message: format!("DB version query failed: {e}"),
            },
        },
        Err(e) => HealthInfo {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            db_ready: false,
            db_path: Some(path),
            db_version: 0,
            message: format!("DB lock failed: {e}"),
        },
    }
}
