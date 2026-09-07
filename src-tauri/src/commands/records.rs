use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::Db;
use crate::domain::records::{self, NewRecord, RecordFilter, RecordPatch, UsageRecordView};

/// 分页查询明细
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(flatten)]
    pub filter: RecordFilter,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PageResult {
    pub rows: Vec<UsageRecordView>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[tauri::command]
pub fn list_records(db: State<'_, Db>, q: PageQuery) -> Result<PageResult, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let page = q.page.unwrap_or(0).max(0);
    let page_size = q.page_size.unwrap_or(50);
    let total = records::count(&conn, &q.filter).map_err(|e| e.to_string())?;
    let rows = records::list_page(&conn, &q.filter, page, page_size).map_err(|e| e.to_string())?;
    Ok(PageResult { rows, total, page, page_size })
}

/// 手动录入一条
#[tauri::command]
pub fn add_record(db: State<'_, Db>, rec: NewRecord) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    records::insert(&conn, &rec, None)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "重复的 request_id".to_string())
}

#[tauri::command]
pub fn update_record(db: State<'_, Db>, id: i64, patch: RecordPatch) -> Result<bool, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    records::update(&conn, id, &patch).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_record(db: State<'_, Db>, id: i64) -> Result<bool, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    records::delete(&conn, id).map_err(|e| e.to_string())
}
