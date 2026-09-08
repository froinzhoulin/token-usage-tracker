//! 用量记录领域: 过滤条件、记录类型、CRUD 与费用换算统一逻辑。

use rusqlite::{params, Connection, Result, Row};
use serde::{Deserialize, Serialize};

use super::price::{estimate_cost_usd, price_for_model_any};

/// 前端传入的通用过滤条件(所有 list/stats/export 命令共用)。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RecordFilter {
    /// ISO 日期起(含), 如 2026-01-01
    pub from: Option<String>,
    /// ISO 日期止(含)
    pub to: Option<String>,
    pub provider_code: Option<String>,
    pub model_name: Option<String>,
    pub project: Option<String>,
    pub tag: Option<String>,
    pub session_keyword: Option<String>,
}

/// 明细页行(供列表与导出)。
#[derive(Debug, Clone, Serialize)]
pub struct UsageRecordView {
    pub id: i64,
    pub recorded_at: String,
    pub source: String,
    pub provider_code: Option<String>,
    pub model_name: Option<String>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// USD 计
    pub cost_usd: Option<f64>,
    pub cost_source: Option<String>,
    pub project: Option<String>,
    pub tags: Option<String>,
    pub note: Option<String>,
}

/// 新记录输入(手动录入 / 导入行统一入口)。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewRecord {
    pub recorded_at: String,
    pub source: String,
    pub provider_code: Option<String>,
    pub model_name: Option<String>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    /// 自带费用(USD)。None 时按单价库自动换算
    pub cost_usd: Option<f64>,
    pub cost_source: Option<String>,
    pub project: Option<String>,
    /// 标签数组
    pub tags: Option<Vec<String>>,
    pub note: Option<String>,
}

/// 汇总一条记录的 token 总数
pub fn total_or(prompt: Option<i64>, completion: Option<i64>) -> Option<i64> {
    match (prompt, completion) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 将任意常见格式的时间统一为固定宽度 UTC ISO8601:
/// `2026-09-08T02:50:09.000Z` (毫秒 3 位 + Z)。
/// 这样字符串比较 == 时间比较; 兼容 'YYYY-MM-DD HH:MM:SS'(视为UTC) / 带T / 带Z / 小数位数不一。
pub fn normalize_recorded_at(input: &str) -> String {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
    let s = input.trim();
    // 尝试完整 ISO(带时区)
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    // 'YYYY-MM-DD HH:MM:SS' / 'YYYY-MM-DDTHH:MM:SS' → 视为 UTC
    let compact = s.replace('T', " ");
    let compact = compact.split('.').next().unwrap_or(&compact).trim();
    if let Ok(ndt) = NaiveDateTime::parse_from_str(compact, "%Y-%m-%d %H:%M:%S") {
        return Utc.from_utc_datetime(&ndt).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    // 'YYYY-MM-DD' → 当天 00:00
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d.and_hms_opt(0, 0, 0).unwrap();
        return Utc.from_utc_datetime(&ndt).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    // 无法解析: 原样返回(极少见, 避免吞数据)
    s.to_string()
}

fn row_to_view(r: &Row) -> rusqlite::Result<UsageRecordView> {
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
}

/// 将本地时区日期 "2026-09-08"(当天 00:00) 转为等价 UTC 毫秒格式,
/// 与 normalize_recorded_at 输出格式一致, 保证字符串比较=时间比较。
fn local_day_start_utc(date: &str) -> String {
    use chrono::{Local, NaiveDate, TimeZone};
    let start = match NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d") {
        Ok(d) => Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()),
        Err(_) => return date.to_string(),
    };
    match start.single() {
        Some(dt) => dt
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => date.to_string(),
    }
}

/// to 语义为开区间: 返回 to 次日 00:00(本地时区)对应的 UTC 时刻。
fn local_day_end_exclusive_utc(date: &str) -> String {
    use chrono::{Duration, Local, NaiveDate, TimeZone};
    let next = match NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d") {
        Ok(d) => d + Duration::days(1),
        Err(_) => return date.to_string(),
    };
    let start = Local.from_local_datetime(&next.and_hms_opt(0, 0, 0).unwrap());
    match start.single() {
        Some(dt) => dt
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => date.to_string(),
    }
}

/// 把过滤条件编译为 SQL WHERE 片段与参数(统一复用)。
pub fn filter_sql(f: &RecordFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conds: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(v) = &f.from {
        conds.push("recorded_at >= ?".to_string());
        params.push(Box::new(local_day_start_utc(v)));
    }
    if let Some(v) = &f.to {
        conds.push("recorded_at < ?".to_string());
        params.push(Box::new(local_day_end_exclusive_utc(v)));
    }
    if let Some(v) = &f.provider_code {
        conds.push("provider_code = ?".to_string());
        params.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.model_name {
        conds.push("model_name = ?".to_string());
        params.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.project {
        conds.push("project = ?".to_string());
        params.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.tag {
        conds.push(
            "EXISTS (SELECT 1 FROM json_each(COALESCE(tags,'[]')) WHERE json_each.value = ?)"
                .to_string(),
        );
        params.push(Box::new(v.clone()));
    }
    if let Some(v) = &f.session_keyword {
        if !v.is_empty() {
            conds.push("(session_id LIKE ? OR request_id LIKE ? OR note LIKE ?)".to_string());
            let pat = format!("%{v}%");
            params.push(Box::new(pat.clone()));
            params.push(Box::new(pat.clone()));
            params.push(Box::new(pat));
        }
    }
    let where_sql = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };
    (where_sql, params)
}

/// 数量(分页用)。
pub fn count(conn: &Connection, f: &RecordFilter) -> Result<i64> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!("SELECT COUNT(*) FROM usage_record {where_sql}");
    conn.query_row(
        &sql,
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        |r| r.get(0),
    )
}

/// 分页列表(新→旧)。
pub fn list_page(
    conn: &Connection,
    f: &RecordFilter,
    page: i64,
    page_size: i64,
) -> Result<Vec<UsageRecordView>> {
    let (where_sql, mut bind) = filter_sql(f);
    let page_size = page_size.clamp(1, 500);
    let offset = page.max(0) * page_size;
    let sql = format!(
        "SELECT id, recorded_at, source, provider_code, model_name, session_id, request_id,
                prompt_tokens, completion_tokens, cached_tokens, total_tokens,
                cost_usd, cost_source, project, tags, note
         FROM usage_record {where_sql}
         ORDER BY recorded_at DESC, id DESC
         LIMIT ?1 OFFSET ?2"
    );
    bind.push(Box::new(page_size));
    bind.push(Box::new(offset));
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map(
            rusqlite::params_from_iter(bind.iter().map(|b| b.as_ref())),
            |r| row_to_view(r),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 单条详情。
pub fn get(conn: &Connection, id: i64) -> Result<Option<UsageRecordView>> {
    let mut stmt = conn.prepare(
        "SELECT id, recorded_at, source, provider_code, model_name, session_id, request_id,
                prompt_tokens, completion_tokens, cached_tokens, total_tokens,
                cost_usd, cost_source, project, tags, note
         FROM usage_record WHERE id = ?1",
    )?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(r) => Ok(Some(row_to_view(r)?)),
        None => Ok(None),
    }
}

/// 插入一条记录; 若提供了 request_id 且已存在则返回 None(去重跳过)。
/// 费用缺省时按价格库自动换算并标注 cost_source=computed。
pub fn insert(
    conn: &Connection,
    rec: &NewRecord,
    batch_id: Option<i64>,
) -> Result<Option<i64>> {
    if let Some(rid) = &rec.request_id {
        if !rid.is_empty() {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_record WHERE request_id = ?1)",
                params![rid],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(None);
            }
        }
    }

    let total = total_or(rec.prompt_tokens, rec.completion_tokens);
    let recorded_at = normalize_recorded_at(&rec.recorded_at);

    // 费用: 记录自带优先; 否则按模型单价换算
    let (cost_usd, cost_source) = match rec.cost_usd {
        Some(v) => {
            let src = rec.cost_source.clone().unwrap_or_else(|| "official".to_string());
            (Some(v), Some(src))
        }
        None => {
            if let Some(model) = &rec.model_name {
                if let Some(price) = price_for_model_any(conn, model)? {
                    let est = estimate_cost_usd(
                        &price,
                        rec.prompt_tokens,
                        rec.completion_tokens,
                        rec.cached_tokens,
                    );
                    match est {
                        Some(v) => (Some(v), Some("computed".to_string())),
                        None => (None, None),
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        }
    };

    let tags_json = match &rec.tags {
        Some(t) => Some(serde_json::to_string(t).unwrap_or_else(|_| "[]".into())),
        None => None,
    };

    conn.execute(
        "INSERT INTO usage_record
            (recorded_at, source, batch_id, provider_code, model_name, session_id, request_id,
             prompt_tokens, completion_tokens, cached_tokens, total_tokens,
             cost_usd, cost_currency, cost_source, project, tags, note)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'USD',?13,?14,?15,?16)",
        params![
            recorded_at,
            rec.source,
            batch_id,
            rec.provider_code,
            rec.model_name,
            rec.session_id,
            rec.request_id,
            rec.prompt_tokens,
            rec.completion_tokens,
            rec.cached_tokens,
            total,
            cost_usd,
            cost_source,
            rec.project,
            tags_json,
            rec.note,
        ],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// 更新部分字段(编辑)。tags 为数组或 None(不修改)。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecordPatch {
    pub recorded_at: Option<String>,
    pub provider_code: Option<String>,
    pub model_name: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub tags: Option<Vec<String>>,
    pub note: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    /// 编辑后是否按最新 token 重算费用
    pub recompute_cost: bool,
}

pub fn update(conn: &Connection, id: i64, patch: &RecordPatch) -> Result<bool> {
    let existing = match get(conn, id)? {
        Some(v) => v,
        None => return Ok(false),
    };
    let recorded_at = normalize_recorded_at(&patch.recorded_at.clone().unwrap_or(existing.recorded_at));
    let provider_code = patch
        .provider_code
        .clone()
        .or(existing.provider_code.clone());
    let model_name = patch.model_name.clone().or(existing.model_name.clone());
    let session_id = patch.session_id.clone().or(existing.session_id.clone());
    let project = patch.project.clone().or(existing.project.clone());
    let note = patch.note.clone().or(existing.note.clone());
    let prompt = patch.prompt_tokens.or(existing.prompt_tokens);
    let completion = patch.completion_tokens.or(existing.completion_tokens);
    let cached = patch.cached_tokens.or(existing.cached_tokens);
    let total = total_or(prompt, completion);

    let tags_json = match &patch.tags {
        Some(t) => Some(serde_json::to_string(t).unwrap_or_else(|_| "[]".into())),
        None => existing.tags.clone(),
    };

    // 费用决定: recompute → 按单价; 否则保留或使用显式 cost_usd
    let (cost_usd, cost_source) = if patch.recompute_cost {
        match &model_name {
            Some(m) => match price_for_model_any(conn, m)? {
                Some(p) => match estimate_cost_usd(&p, prompt, completion, cached) {
                    Some(v) => (Some(v), Some("computed".to_string())),
                    None => (existing.cost_usd, existing.cost_source.clone()),
                },
                None => (existing.cost_usd, existing.cost_source.clone()),
            },
            None => (existing.cost_usd, existing.cost_source.clone()),
        }
    } else if patch.cost_usd.is_some() {
        (patch.cost_usd, Some("manual".to_string()))
    } else {
        (existing.cost_usd, existing.cost_source.clone())
    };

    conn.execute(
        "UPDATE usage_record SET
            recorded_at=?1, provider_code=?2, model_name=?3, session_id=?4,
            prompt_tokens=?5, completion_tokens=?6, cached_tokens=?7, total_tokens=?8,
            cost_usd=?9, cost_source=?10, project=?11, tags=?12, note=?13
         WHERE id=?14",
        params![
            recorded_at,
            provider_code,
            model_name,
            session_id,
            prompt,
            completion,
            cached,
            total,
            cost_usd,
            cost_source,
            project,
            tags_json,
            note,
            id,
        ],
    )?;
    Ok(true)
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute("DELETE FROM usage_record WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// 为 cost_usd 为空(且模型有价)的记录按最新单价换算, 返回更新条数。
pub fn recompute_missing(conn: &Connection) -> Result<i64> {
    let missing: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM usage_record
             WHERE (cost_usd IS NULL OR cost_source = 'computed')
               AND model_name IS NOT NULL AND model_name <> ''",
        )?;
        let ids = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };
    let mut updated = 0i64;
    for id in missing {
        let view = match get(conn, id)? {
            Some(v) => v,
            None => continue,
        };
        let Some(model) = &view.model_name else { continue };
        let Some(price) = price_for_model_any(conn, model)? else { continue };
        let Some(est) = estimate_cost_usd(
            &price,
            view.prompt_tokens,
            view.completion_tokens,
            view.cached_tokens,
        ) else {
            continue;
        };
        conn.execute(
            "UPDATE usage_record SET cost_usd = ?1, cost_source = 'computed' WHERE id = ?2",
            params![est, id],
        )?;
        updated += 1;
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_sql_builds_conditions() {
        let f = RecordFilter {
            from: Some("2026-01-01".into()),
            provider_code: Some("deepseek".into()),
            ..Default::default()
        };
        let (sql, params) = filter_sql(&f);
        assert!(sql.contains("recorded_at >= ?"));
        assert!(sql.contains("provider_code = ?"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn total_computes_correctly() {
        assert_eq!(total_or(Some(10), Some(20)), Some(30));
        assert_eq!(total_or(Some(10), None), Some(10));
        assert_eq!(total_or(None, None), None);
    }

    #[test]
    fn insert_dedup_by_request_id() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let rec = NewRecord {
            recorded_at: "2026-09-01T00:00:00Z".into(),
            source: "manual".into(),
            model_name: Some("deepseek-v4-flash".into()),
            prompt_tokens: Some(1000),
            completion_tokens: Some(500),
            request_id: Some("req-1".into()),
            ..Default::default()
        };
        assert!(insert(&conn, &rec, None).unwrap().is_some());
        assert!(insert(&conn, &rec, None).unwrap().is_none());
        assert_eq!(count(&conn, &RecordFilter::default()).unwrap(), 1);
    }

    #[test]
    fn insert_computes_cost_when_missing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let rec = NewRecord {
            recorded_at: "2026-09-01T00:00:00Z".into(),
            source: "manual".into(),
            model_name: Some("deepseek-v4-flash".into()),
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(1_000_000),
            cost_usd: None,
            ..Default::default()
        };
        let id = insert(&conn, &rec, None).unwrap().unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        // 0.44 + 1.32 = 1.76 USD (缓存为空)
        let c = row.cost_usd.unwrap();
        assert!((c - 1.76).abs() < 1e-6, "got {c}");
        assert_eq!(row.cost_source.as_deref(), Some("computed"));
    }

    #[test]
    fn alias_model_name_gets_priced() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        // deepseek-chat 无内置价, 别名映射到 deepseek-v4-flash 计价
        let rec = NewRecord {
            recorded_at: "2026-09-01T00:00:00Z".into(),
            source: "manual".into(),
            model_name: Some("deepseek-chat".into()),
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(1_000_000),
            cost_usd: None,
            ..Default::default()
        };
        let id = insert(&conn, &rec, None).unwrap().unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        let c = row.cost_usd.unwrap();
        assert!((c - 1.76).abs() < 1e-6, "got {c}");
        assert_eq!(row.cost_source.as_deref(), Some("computed"));
    }
}
