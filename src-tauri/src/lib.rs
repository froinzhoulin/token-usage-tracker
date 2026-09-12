pub mod claude_code_watcher;
pub mod codex_watcher;
pub mod collector;
pub mod commands;
pub mod db;
pub mod domain;
pub mod dsh_watcher;
pub mod proxy;
pub mod workbuddy_watcher;

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

            // 启动 DSH 本机用量自动检测
            {
                let dsh_home = std::env::var("DSH_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var("HOME")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".dsh"))
                    })
                    .or_else(|| {
                        std::env::var("USERPROFILE")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".dsh"))
                    });
                if let Some(home) = dsh_home {
                    if home.join("storages").join("session_projcache").join("sessions").is_dir() {
                        let stop = Arc::new(AtomicBool::new(false));
                        let conn = db.shared();
                        let _ = dsh_watcher::start_watcher(
                            conn,
                            home,
                            dsh_watcher::DEFAULT_POLL_MS,
                            Arc::clone(&stop),
                        );
                        app.manage(DshWatcherState { stop });
                    }
                }
            }

            // 启动 Claude Code 本机用量自动检测(对齐 DSH watcher 体验)
            {
                let claude_home = std::env::var("CLAUDE_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var("USERPROFILE")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".claude"))
                    })
                    .or_else(|| {
                        std::env::var("HOME")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".claude"))
                    });
                let mut cc_info = claude_code_watcher::WatcherInfo {
                    claude_home: String::new(),
                    started: false,
                    error: None,
                };
                if let Some(home) = claude_home {
                    cc_info.claude_home = home.display().to_string();
                    if home.join("projects").is_dir() {
                        let stop = Arc::new(AtomicBool::new(false));
                        let conn = db.shared();
                        match claude_code_watcher::start_watcher(
                            conn,
                            home.clone(),
                            claude_code_watcher::DEFAULT_POLL_MS,
                            Arc::clone(&stop),
                        ) {
                            Ok(()) => {
                                cc_info.started = true;
                                app.manage(ClaudeCodeWatcherState { stop });
                            }
                            Err(e) => cc_info.error = Some(e),
                        }
                    } else {
                        cc_info.error = Some("未找到 ~/.claude/projects 目录".into());
                    }
                } else {
                    cc_info.error = Some("未定位到 ~/.claude 路径".into());
                }
                app.manage(ClaudeCodeWatcherInfo(Arc::new(Mutex::new(Some(cc_info)))));
            }

            // 启动 Codex 本机用量自动检测(读取 ~/.codex/sessions 的 rollout jsonl)
            {
                let codex_home = std::env::var("CODEX_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var("USERPROFILE")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".codex"))
                    })
                    .or_else(|| {
                        std::env::var("HOME")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".codex"))
                    });
                let mut cx_info = codex_watcher::WatcherInfo {
                    codex_home: String::new(),
                    started: false,
                    error: None,
                };
                if let Some(home) = codex_home {
                    cx_info.codex_home = home.display().to_string();
                    if home.join("sessions").is_dir() {
                        let stop = Arc::new(AtomicBool::new(false));
                        let conn = db.shared();
                        match codex_watcher::start_watcher(
                            conn,
                            home.clone(),
                            codex_watcher::DEFAULT_POLL_MS,
                            Arc::clone(&stop),
                        ) {
                            Ok(()) => {
                                cx_info.started = true;
                                app.manage(CodexWatcherState { stop });
                            }
                            Err(e) => cx_info.error = Some(e),
                        }
                    } else {
                        cx_info.error = Some("未找到 ~/.codex/sessions 目录".into());
                    }
                } else {
                    cx_info.error = Some("未定位到 ~/.codex 路径".into());
                }
                app.manage(CodexWatcherInfo(Arc::new(Mutex::new(Some(cx_info)))));
            }

            // 启动 WorkBuddy 本机用量自动检测(读取 ~/.workbuddy/projects 的会话 jsonl)
            {
                let wb_home = std::env::var("WORKBUDDY_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var("USERPROFILE")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".workbuddy"))
                    })
                    .or_else(|| {
                        std::env::var("HOME")
                            .ok()
                            .map(|h| std::path::PathBuf::from(h).join(".workbuddy"))
                    });
                let mut wb_info = workbuddy_watcher::WatcherInfo {
                    workbuddy_home: String::new(),
                    started: false,
                    error: None,
                };
                if let Some(home) = wb_home {
                    wb_info.workbuddy_home = home.display().to_string();
                    if home.join("projects").is_dir() {
                        let stop = Arc::new(AtomicBool::new(false));
                        let conn = db.shared();
                        match workbuddy_watcher::start_watcher(
                            conn,
                            home.clone(),
                            workbuddy_watcher::DEFAULT_POLL_MS,
                            Arc::clone(&stop),
                        ) {
                            Ok(()) => {
                                wb_info.started = true;
                                app.manage(WorkBuddyWatcherState { stop });
                            }
                            Err(e) => wb_info.error = Some(e),
                        }
                    } else {
                        wb_info.error = Some("未找到 ~/.workbuddy/projects 目录".into());
                    }
                } else {
                    wb_info.error = Some("未定位到 ~/.workbuddy 路径".into());
                }
                app.manage(WorkBuddyWatcherInfo(Arc::new(Mutex::new(Some(wb_info)))));
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
            commands::stats::usage_hourly_trend,
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
            commands::claude_code::claude_code_status,
            commands::codex::codex_status,
            commands::workbuddy::workbuddy_status,
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

/// DSH watcher 运行状态(managed state)
pub struct DshWatcherState {
    pub stop: Arc<AtomicBool>,
}

impl Drop for DshWatcherState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Claude Code watcher 运行状态(managed state)
pub struct ClaudeCodeWatcherState {
    pub stop: Arc<AtomicBool>,
}

impl Drop for ClaudeCodeWatcherState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Claude Code watcher 启动信息(供前端展示)
pub struct ClaudeCodeWatcherInfo(pub Arc<Mutex<Option<claude_code_watcher::WatcherInfo>>>);

/// Codex watcher 运行状态(managed state)
pub struct CodexWatcherState {
    pub stop: Arc<AtomicBool>,
}

impl Drop for CodexWatcherState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Codex watcher 启动信息(供前端展示)
pub struct CodexWatcherInfo(pub Arc<Mutex<Option<codex_watcher::WatcherInfo>>>);

/// WorkBuddy watcher 运行状态(managed state)
pub struct WorkBuddyWatcherState {
    pub stop: Arc<AtomicBool>,
}

impl Drop for WorkBuddyWatcherState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// WorkBuddy watcher 启动信息(供前端展示)
pub struct WorkBuddyWatcherInfo(pub Arc<Mutex<Option<workbuddy_watcher::WatcherInfo>>>);
