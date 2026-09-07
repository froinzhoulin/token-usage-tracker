pub mod commands;
pub mod db;
pub mod domain;

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
            app.manage(db::Db::new(conn, db_path));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
