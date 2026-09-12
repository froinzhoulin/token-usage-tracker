//! Codex watcher 状态查询(供前端看板展示)。

use tauri::State;

use crate::{codex_watcher::WatcherInfo, CodexWatcherInfo};

/// 返回 Codex watcher 启动信息(路径/是否成功/错误原因)。
#[tauri::command]
pub fn codex_status(info: State<'_, CodexWatcherInfo>) -> Result<WatcherInfo, String> {
    let guard = info.0.lock().map_err(|e| e.to_string())?;
    guard
        .clone()
        .ok_or_else(|| "Codex watcher 信息尚未初始化".to_string())
}
