//! WorkBuddy watcher 状态查询(供前端看板展示)。

use tauri::State;

use crate::{workbuddy_watcher::WatcherInfo, WorkBuddyWatcherInfo};

/// 返回 WorkBuddy watcher 启动信息(路径/是否成功/错误原因)。
#[tauri::command]
pub fn workbuddy_status(info: State<'_, WorkBuddyWatcherInfo>) -> Result<WatcherInfo, String> {
    let guard = info.0.lock().map_err(|e| e.to_string())?;
    guard
        .clone()
        .ok_or_else(|| "WorkBuddy watcher 信息尚未初始化".to_string())
}
