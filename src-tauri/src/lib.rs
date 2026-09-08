pub mod collector;
pub mod commands;
pub mod db;
pub mod domain;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 初始化本地 SQLite（存放于系统应用数据目录，非程序安装目录）
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("tracker.db");
            let conn = db::open(&db_path)?;
            db::migrate(&conn)?;
            let db = db::Db::new(conn, db_path.clone());
            let shared_conn = db.shared();

            // 启动本地用量收集端点(仅 127.0.0.1)
            {
                let probe = rusqlite::Connection::open(&db_path)?;
                let port: u16 = probe
                    .query_row(
                        "SELECT value FROM kv_settings WHERE key='collector_port'",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(8765);
                let stop = Arc::new(AtomicBool::new(false));
                let info = collector::start(shared_conn, port, Arc::clone(&stop));
                let info = match info {
                    Ok(i) => i,
                    Err(e) => collector::CollectorInfo {
                        port,
                        started: false,
                        error: Some(e),
                    },
                };
                app.manage(CollectorState {
                    info: Arc::new(Mutex::new(Some(info))),
                    stop,
                });
            }

            app.manage(db);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::collector_status::collector_status,
            commands::health::health,
            commands::records::list_records,
            commands::records::add_record,
            commands::records::update_record,
            commands::records::delete_record,
            commands::stats::usage_overview,
            commands::stats::usage_trend,
            commands::stats::usage_distribution,
            commands::stats::dashboard,
            commands::stats::known_models,
            commands::price::list_prices,
            commands::price::upsert_custom_price,
            commands::price::recompute_missing_costs,
            commands::import_::preview_csv,
            commands::import_::import_csv,
            commands::export::export_data,
            commands::export::write_text_file,
            commands::settings::get_settings,
            commands::settings::set_settings,
            commands::settings::backup_db,
            commands::settings::restore_db,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// collector 运行状态(managed state)
pub struct CollectorState {
    pub info: Arc<Mutex<Option<collector::CollectorInfo>>>,
    pub stop: Arc<AtomicBool>,
}

impl Drop for CollectorState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
