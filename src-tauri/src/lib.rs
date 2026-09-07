mod commands;
mod db;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
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
        .invoke_handler(tauri::generate_handler![commands::health::health])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
