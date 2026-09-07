use serde::Deserialize;
use tauri::State;

use crate::db::Db;
use crate::domain::export as de;
use crate::domain::records::RecordFilter;

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(flatten)]
    pub filter: RecordFilter,
    /// csv | json
    pub format: String,
}

/// 生成导出内容(不落盘, 由前端保存)。
#[tauri::command]
pub fn export_data(
    db: State<'_, Db>,
    q: ExportQuery,
) -> Result<de::ExportPayload, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    match q.format.as_str() {
        "json" => de::to_json(&conn, &q.filter),
        _ => de::to_csv(&conn, &q.filter),
    }
}

/// 把文本内容写入指定路径(前端经 save dialog 选好路径后调用)。
#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content.as_bytes()).map_err(|e| format!("写入失败: {e}"))
}
