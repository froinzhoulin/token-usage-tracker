use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::Db;
use crate::domain::records::RecordFilter;
use crate::domain::stats::{self, DistBucket, Overview, TrendPoint};

#[tauri::command]
pub fn usage_overview(db: State<'_, Db>, filter: RecordFilter) -> Result<Overview, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    stats::overview(&conn, &filter).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn usage_trend(db: State<'_, Db>, filter: RecordFilter) -> Result<Vec<TrendPoint>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    stats::trend(&conn, &filter).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct DistQuery {
    #[serde(flatten)]
    pub filter: RecordFilter,
    pub dimension: String,
    pub limit: Option<i64>,
}

#[tauri::command]
pub fn usage_distribution(db: State<'_, Db>, q: DistQuery) -> Result<Vec<DistBucket>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    stats::distribution(&conn, &q.filter, &q.dimension, q.limit.unwrap_or(10))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn known_models(db: State<'_, Db>) -> Result<Vec<String>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    stats::known_models(&conn).map_err(|e| e.to_string())
}

/// 汇总返回(看板一次拉全: overview + trend + distributions)
#[derive(Debug, Serialize)]
pub struct DashboardData {
    pub overview: Overview,
    pub trend: Vec<TrendPoint>,
    pub by_model: Vec<DistBucket>,
    pub by_provider: Vec<DistBucket>,
    pub by_project: Vec<DistBucket>,
    pub known_models: Vec<String>,
}

#[tauri::command]
pub fn dashboard(db: State<'_, Db>, filter: RecordFilter) -> Result<DashboardData, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    Ok(DashboardData {
        overview: stats::overview(&conn, &filter).map_err(|e| e.to_string())?,
        trend: stats::trend(&conn, &filter).map_err(|e| e.to_string())?,
        by_model: stats::distribution(&conn, &filter, "model_name", 10).map_err(|e| e.to_string())?,
        by_provider: stats::distribution(&conn, &filter, "provider_code", 10)
            .map_err(|e| e.to_string())?,
        by_project: stats::distribution(&conn, &filter, "project", 10).map_err(|e| e.to_string())?,
        known_models: stats::known_models(&conn).map_err(|e| e.to_string())?,
    })
}
