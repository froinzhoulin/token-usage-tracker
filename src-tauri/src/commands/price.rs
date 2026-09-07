use serde::Deserialize;
use tauri::State;

use crate::db::Db;
use crate::domain::price::{self, ModelPriceView};

#[tauri::command]
pub fn list_prices(db: State<'_, Db>) -> Result<Vec<ModelPriceView>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    price::list_all(&conn).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct CustomPrice {
    pub provider_code: String,
    pub provider_name: String,
    pub model_name: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cached_input_per_mtok: Option<f64>,
}

#[tauri::command]
pub fn upsert_custom_price(db: State<'_, Db>, p: CustomPrice) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    price::upsert_custom(
        &conn,
        &p.provider_code,
        &p.provider_name,
        &p.model_name,
        p.input_per_mtok,
        p.output_per_mtok,
        p.cached_input_per_mtok,
    )
    .map_err(|e| e.to_string())
}

/// 为缺失费用(computed 缺失)的记录批量重算, 返回更新条数。
#[tauri::command]
pub fn recompute_missing_costs(db: State<'_, Db>) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    crate::domain::records::recompute_missing(&conn).map_err(|e| e.to_string())
}
