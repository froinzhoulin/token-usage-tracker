//! Claude Code watcher 状态查询(供前端看板展示)。

use tauri::State;

use crate::{claude_code_watcher::WatcherInfo, ClaudeCodeWatcherInfo};

/// 返回 Claude Code watcher 启动信息(路径/是否成功/错误原因)。
#[tauri::command]
pub fn claude_code_status(info: State<'_, ClaudeCodeWatcherInfo>) -> Result<WatcherInfo, String> {
    let guard = info.0.lock().map_err(|e| e.to_string())?;
    guard
        .clone()
        .ok_or_else(|| "Claude Code watcher 信息尚未初始化".to_string())
}