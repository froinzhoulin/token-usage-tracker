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

#[cfg(test)]
mod tests {
    use super::*;

    /// 固化前端 listRecords 的载荷形态: 过滤字段必须平铺在 q 顶层,
    /// 由 #[serde(flatten)] 承接。这是"来源/日期筛选生效"的关键契约。
    #[test]
    fn page_query_deserializes_flattened_filter() {
        let q: PageQuery = serde_json::from_str(
            r#"{"source":"claude_code","from":"2026-09-10","to":"2026-09-10","page":0,"page_size":15}"#,
        )
        .expect("平铺载荷应能解析");
        assert_eq!(q.filter.source.as_deref(), Some("claude_code"));
        assert_eq!(q.filter.from.as_deref(), Some("2026-09-10"));
        assert_eq!(q.page, Some(0));
        assert_eq!(q.page_size, Some(15));
    }

    /// 已知陷阱: 若前端把 filter 包成嵌套对象, serde 会静默丢弃(不报错),
    /// 表现为"筛选点了没反应"。此测试固化该行为, 防止前端回退到旧写法。
    #[test]
    fn nested_filter_silently_dropped() {
        let q: PageQuery =
            serde_json::from_str(r#"{"filter":{"source":"dsh"},"page":0,"page_size":15}"#)
                .expect("嵌套形态不报错");
        assert_eq!(q.filter.source, None, "嵌套 filter 被静默丢弃(即筛选失效)");
    }
}
