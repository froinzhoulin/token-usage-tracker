//! CSV 导入: 读取文件 → 依据列映射构建 NewRecord → 批量入库。
//! 返回批次统计(总行/成功/失败/跳过), 失败原因逐行记录。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::records::{insert, NewRecord};

/// 前端传来的列映射: 源CSV列名 → 目标字段
/// 目标字段白名单见 `TARGET_FIELDS`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ColumnMapping {
    pub map: HashMap<String, String>,
    /// 导入批次的统一来源(如文件名/来源标签), 落到 note 或忽略
    pub batch_source: Option<String>,
}

/// 允许映射到的目标字段
pub const TARGET_FIELDS: &[&str] = &[
    "recorded_at",
    "provider_code",
    "model_name",
    "session_id",
    "request_id",
    "prompt_tokens",
    "completion_tokens",
    "cached_tokens",
    "cost_usd",
    "project",
    "tags",
    "note",
];

/// 导入结果
#[derive(Debug, Default, Serialize)]
pub struct ImportResult {
    pub total_rows: usize,
    pub ok_rows: usize,
    pub failed_rows: usize,
    pub skipped_rows: usize,
    pub batch_id: Option<i64>,
    pub errors: Vec<String>,
}

/// 执行导入。任一必填字段缺失或数值非法 → 记入 errors。
pub fn import_csv(
    conn: &Connection,
    path: &str,
    mapping: &ColumnMapping,
    file_name: &str,
) -> Result<ImportResult, String> {
    // 读取并解析
    let content = std::fs::read_to_string(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());

    let headers = match rdr.headers() {
        Ok(h) => h.clone(),
        Err(e) => return Err(format!("CSV 表头解析失败: {e}")),
    };

    // 建立 源列名→目标字段 的索引表
    let mut col_target: HashMap<String, String> = HashMap::new();
    for (i, h) in headers.iter().enumerate() {
        let hkey = h.to_string();
        if let Some(target) = mapping.map.get(&hkey).or_else(|| mapping.map.get(&i.to_string())) {
            if TARGET_FIELDS.contains(&target.as_str()) {
                col_target.insert(hkey.clone(), target.clone());
            }
        }
    }
    // 兼容序号键(前端可能给 "0","1"...)
    for (i, h) in headers.iter().enumerate() {
        let idx = i.to_string();
        if let Some(target) = mapping.map.get(&idx) {
            if TARGET_FIELDS.contains(&target.as_str()) {
                col_target.entry(h.to_string()).or_insert(target.clone());
            }
        }
    }

    let mut result = ImportResult::default();
    let required_have = ["recorded_at", "model_name"]
        .iter()
        .any(|f| col_target.values().any(|v| v == f));

    let tx = conn.unchecked_transaction().map_err(|e| format!("事务启动失败: {e}"))?;

    let mut batch_id: Option<i64> = None;
    // 先建批次(即使空也建, 便于定位)
    tx.execute(
        "INSERT INTO import_batch(file_name, total_rows, ok_rows, failed_rows, mapping_json)
         VALUES(?1, 0, 0, 0, ?2)",
        rusqlite::params![
            file_name,
            serde_json::to_string(&mapping.map).unwrap_or_default()
        ],
    )
    .map_err(|e| format!("批次写入失败: {e}"))?;
    batch_id = Some(tx.last_insert_rowid());

    for (line_no, record) in rdr.records().enumerate() {
        let csv_row = record.map_err(|e| format!("第 {} 行解析错误: {e}", line_no + 2))?;
        result.total_rows += 1;

        if !required_have {
            result.failed_rows += 1;
            result.errors.push(format!(
                "行 {}: 缺少必填字段映射(recorded_at / model_name)",
                line_no + 2
            ));
            continue;
        }

        let mut rec = NewRecord {
            source: "import_csv".to_string(),
            ..Default::default()
        };
        if let Some(src) = &mapping.batch_source {
            rec.note = Some(src.clone());
        }

        let mut err: Option<String> = None;
        for (col, target) in &col_target {
            // 按列名取值
            let raw = headers.iter().position(|h| h == col).and_then(|pos| {
                csv_row.get(pos).map(|v| v.trim().to_string())
            });
            let Some(value) = raw.filter(|v| !v.is_empty()) else {
                continue;
            };
            let parsed = parse_field(&mut rec, target, &value);
            if let Err(e) = parsed {
                err = Some(format!("列[{col}] {e}"));
                break;
            }
        }

        if let Some(e) = err {
            result.failed_rows += 1;
            result.errors.push(format!("行 {}: {e}", line_no + 2));
            continue;
        }
        if rec.recorded_at.is_empty() {
            result.failed_rows += 1;
            result.errors.push(format!(
                "行 {}: recorded_at 为空",
                line_no + 2
            ));
            continue;
        }

        match insert(&tx, &rec, batch_id) {
            Ok(Some(_)) => result.ok_rows += 1,
            Ok(None) => result.skipped_rows += 1,
            Err(e) => {
                result.failed_rows += 1;
                result.errors.push(format!("行 {}: 入库失败 {e}", line_no + 2));
            }
        }
    }

    // 更新批次统计
    tx.execute(
        "UPDATE import_batch SET total_rows=?1, ok_rows=?2, failed_rows=?3 WHERE id=?4",
        rusqlite::params![
            result.total_rows as i64,
            result.ok_rows as i64,
            result.failed_rows as i64,
            batch_id.unwrap_or(0)
        ],
    )
    .map_err(|e| format!("批次统计更新失败: {e}"))?;

    tx.commit().map_err(|e| format!("提交失败: {e}"))?;

    result.batch_id = batch_id;
    // 错误信息截断避免 IPC 过大
    if result.errors.len() > 200 {
        result.errors.truncate(200);
        result.errors.push("...错误过多, 已截断".to_string());
    }
    Ok(result)
}

fn parse_field(rec: &mut NewRecord, target: &str, value: &str) -> Result<(), String> {
    match target {
        "recorded_at" => rec.recorded_at = value.to_string(),
        "provider_code" => rec.provider_code = Some(value.to_string()),
        "model_name" => rec.model_name = Some(value.to_string()),
        "session_id" => rec.session_id = Some(value.to_string()),
        "request_id" => rec.request_id = Some(value.to_string()),
        "prompt_tokens" => rec.prompt_tokens = Some(parse_i64(value)?),
        "completion_tokens" => rec.completion_tokens = Some(parse_i64(value)?),
        "cached_tokens" => rec.cached_tokens = Some(parse_i64(value)?),
        "cost_usd" => rec.cost_usd = Some(parse_f64(value)?),
        "project" => rec.project = Some(value.to_string()),
        "tags" => {
            // 支持 "a;b" / "a,b" / JSON 数组
            rec.tags = Some(
                value
                    .split([';', ','])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        }
        "note" => {
            if rec.note.is_none() {
                rec.note = Some(value.to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_i64(v: &str) -> Result<i64, String> {
    v.parse::<i64>()
        .map_err(|_| format!("数值格式非法: '{v}'"))
}

fn parse_f64(v: &str) -> Result<f64, String> {
    v.parse::<f64>()
        .map_err(|_| format!("金额格式非法: '{v}'"))
}
