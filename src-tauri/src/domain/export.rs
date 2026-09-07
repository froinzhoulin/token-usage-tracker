//! 明细导出: 按过滤条件导出 CSV(UTF-8 BOM, Excel 友好) 或 JSON。
//! 金额列以 USD 输出, 币种说明见文件头注释。

use rusqlite::Connection;
use serde::Serialize;

use super::records::{filter_sql, UsageRecordView};

#[derive(Serialize)]
pub struct ExportPayload {
    pub file_name: String,
    /// 文件内容
    pub content: String,
    pub rows: usize,
}

const COLS: [&str; 15] = [
    "id", "recorded_at", "source", "provider_code", "model_name", "session_id", "request_id",
    "prompt_tokens", "completion_tokens", "cached_tokens", "total_tokens", "cost_usd",
    "cost_source", "project", "tags",
];

pub fn to_csv(conn: &Connection, f: &super::records::RecordFilter) -> Result<ExportPayload, String> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!(
        "SELECT id, recorded_at, source, provider_code, model_name, session_id, request_id,
                prompt_tokens, completion_tokens, cached_tokens, total_tokens,
                cost_usd, cost_source, project, tags
         FROM usage_record {where_sql}
         ORDER BY recorded_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok(UsageRecordView {
                    id: r.get(0)?,
                    recorded_at: r.get(1)?,
                    source: r.get(2)?,
                    provider_code: r.get(3)?,
                    model_name: r.get(4)?,
                    session_id: r.get(5)?,
                    request_id: r.get(6)?,
                    prompt_tokens: r.get(7)?,
                    completion_tokens: r.get(8)?,
                    cached_tokens: r.get(9)?,
                    total_tokens: r.get(10)?,
                    cost_usd: r.get(11)?,
                    cost_source: r.get(12)?,
                    project: r.get(13)?,
                    tags: r.get(14)?,
                    note: None,
                })
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut wtr = csv::WriterBuilder::new().from_writer(vec![]);
    wtr.write_record(COLS).map_err(|e| e.to_string())?;
    for r in &rows {
        wtr.write_record([
            r.id.to_string(),
            r.recorded_at.clone(),
            r.source.clone(),
            opt(&r.provider_code),
            opt(&r.model_name),
            opt(&r.session_id),
            opt(&r.request_id),
            opt_i64(r.prompt_tokens),
            opt_i64(r.completion_tokens),
            opt_i64(r.cached_tokens),
            opt_i64(r.total_tokens),
            opt_f64(r.cost_usd),
            opt(&r.cost_source),
            opt(&r.project),
            opt(&r.tags),
        ])
        .map_err(|e| e.to_string())?;
    }
    let body = wtr.into_inner().map_err(|e| e.to_string())?;
    // UTF-8 BOM
    let mut content = Vec::with_capacity(body.len() + 3);
    content.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    content.extend_from_slice(&body);
    let content = String::from_utf8_lossy(&content).to_string();

    Ok(ExportPayload {
        file_name: "token-usage-records.csv".to_string(),
        content,
        rows: rows.len(),
    })
}

pub fn to_json(conn: &Connection, f: &super::records::RecordFilter) -> Result<ExportPayload, String> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!(
        "SELECT id, recorded_at, source, provider_code, model_name, session_id, request_id,
                prompt_tokens, completion_tokens, cached_tokens, total_tokens,
                cost_usd, cost_source, project, tags, note
         FROM usage_record {where_sql}
         ORDER BY recorded_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok(UsageRecordView {
                    id: r.get(0)?,
                    recorded_at: r.get(1)?,
                    source: r.get(2)?,
                    provider_code: r.get(3)?,
                    model_name: r.get(4)?,
                    session_id: r.get(5)?,
                    request_id: r.get(6)?,
                    prompt_tokens: r.get(7)?,
                    completion_tokens: r.get(8)?,
                    cached_tokens: r.get(9)?,
                    total_tokens: r.get(10)?,
                    cost_usd: r.get(11)?,
                    cost_source: r.get(12)?,
                    project: r.get(13)?,
                    tags: r.get(14)?,
                    note: r.get(15)?,
                })
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let content =
        serde_json::to_string_pretty(&rows).map_err(|e| format!("JSON 序列化失败: {e}"))?;
    Ok(ExportPayload {
        file_name: "token-usage-records.json".to_string(),
        content,
        rows: rows.len(),
    })
}

fn opt(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}
fn opt_i64(v: Option<i64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}
fn opt_f64(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.6}")).unwrap_or_default()
}
