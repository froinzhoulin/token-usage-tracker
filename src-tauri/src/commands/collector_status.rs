use serde::Serialize;

/// 前端查询 collector 运行状态
#[derive(Serialize)]
pub struct CollectorStatus {
    pub port: u16,
    pub started: bool,
    pub error: Option<String>,
    pub base_url: String,
}

#[tauri::command]
pub fn collector_status(
    state: tauri::State<'_, crate::CollectorState>,
) -> CollectorStatus {
    let guard = state.info.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(info) => CollectorStatus {
            port: info.port,
            started: info.started,
            error: info.error.clone(),
            base_url: format!("http://127.0.0.1:{}", info.port),
        },
        None => CollectorStatus {
            port: 0,
            started: false,
            error: Some("collector 未初始化".into()),
            base_url: String::new(),
        },
    }
}
