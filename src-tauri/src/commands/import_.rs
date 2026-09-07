//! 导入命令: 选择文件(前端 dialog) → preview → 提交导入。

use tauri::State;

use crate::db::Db;
use crate::domain::import::{self, ColumnMapping, ImportResult};

/// CSV 表头预览(前端据此生成字段映射 UI)
#[derive(serde::Serialize)]
pub struct CsvPreview {
    pub headers: Vec<String>,
    /// 前若干行原始值
    pub sample_rows: Vec<Vec<String>>,
    pub total_preview_rows: usize,
}

#[tauri::command]
pub fn preview_csv(db: State<'_, Db>, path: String, limit: Option<usize>) -> Result<CsvPreview, String> {
    let _ = db; // 预览不触碰数据库
    let limit = limit.unwrap_or(20).clamp(1, 200);
    let content = std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    let headers = match rdr.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        Err(e) => return Err(format!("CSV 表头解析失败: {e}")),
    };
    let mut sample_rows: Vec<Vec<String>> = Vec::new();
    let mut count = 0usize;
    for rec in rdr.records() {
        let rec = rec.map_err(|e| format!("CSV 解析失败: {e}"))?;
        sample_rows.push(rec.iter().map(|s| s.to_string()).collect());
        count += 1;
        if sample_rows.len() >= limit {
            break;
        }
    }
    Ok(CsvPreview { headers, sample_rows, total_preview_rows: count })
}

#[tauri::command]
pub fn import_csv(
    db: State<'_, Db>,
    path: String,
    mapping: ColumnMapping,
    file_name: String,
) -> Result<ImportResult, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    import::import_csv(&conn, &path, &mapping, &file_name)
}
