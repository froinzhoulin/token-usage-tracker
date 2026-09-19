//! Hermes Agent watcher 状态查询(供前端看板展示)。
//!
//! 这里实时按当前设置解析数据目录, 而不是读启动时缓存的信息 ——
//! 设置页改完立刻能看到新路径是否可用, 无需重启。

use tauri::State;

use crate::db::Db;
use crate::hermes_watcher::{self, WatcherInfo};
use crate::HermesWatcherState;

/// 返回 Hermes Agent watcher 运行信息(生效路径/是否运行/错误或提示)。
#[tauri::command]
pub fn hermes_status(db: State<'_, Db>, state: State<'_, HermesWatcherState>) -> Result<WatcherInfo, String> {
    let cfg = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        hermes_watcher::load_config(&conn)
    };
    let mut info = hermes_watcher::info_from(&cfg, state.start_error.is_none());
    if let Some(e) = &state.start_error {
        info.error = Some(e.clone());
    }
    Ok(info)
}
