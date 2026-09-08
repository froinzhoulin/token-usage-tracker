//! 统计聚合: 汇总卡片、时间趋势、维度分布。
//! 所有金额均为 USD(存储口径); 展示币种换算在前端完成(依据 settings 汇率)。

use rusqlite::{Connection, Result};
use serde::Serialize;

use super::records::{filter_sql, RecordFilter};

/// 看板顶部汇总指标
#[derive(Debug, Default, Serialize)]
pub struct Overview {
    pub record_count: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: Option<f64>,
    pub day_count: i64,
    pub model_count: i64,
    /// 覆盖的厂商数
    pub provider_count: i64,
}

pub fn overview(conn: &Connection, f: &RecordFilter) -> Result<Overview> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(prompt_tokens),0),
                COALESCE(SUM(completion_tokens),0),
                COALESCE(SUM(cached_tokens),0),
                COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0),
                SUM(cost_usd),
                COUNT(DISTINCT date(recorded_at, 'localtime')),
                COUNT(DISTINCT model_name),
                COUNT(DISTINCT provider_code)
         FROM usage_record {where_sql}"
    );
    conn.query_row(
        &sql,
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        |r| {
            Ok(Overview {
                record_count: r.get(0)?,
                prompt_tokens: r.get(1)?,
                completion_tokens: r.get(2)?,
                cached_tokens: r.get(3)?,
                total_tokens: r.get(4)?,
                cost_usd: r.get(5)?,
                day_count: r.get(6)?,
                model_count: r.get(7)?,
                provider_count: r.get(8)?,
            })
        },
    )
}

/// 趋势序列中的一个点(按日)
#[derive(Debug, Serialize)]
pub struct TrendPoint {
    pub day: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: Option<f64>,
    pub record_count: i64,
}

pub fn trend(conn: &Connection, f: &RecordFilter) -> Result<Vec<TrendPoint>> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!(
        "SELECT date(recorded_at, 'localtime') AS day,
                COALESCE(SUM(prompt_tokens),0),
                COALESCE(SUM(completion_tokens),0),
                COALESCE(SUM(cached_tokens),0),
                COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0),
                SUM(cost_usd),
                COUNT(*)
         FROM usage_record {where_sql}
         GROUP BY day ORDER BY day ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok(TrendPoint {
                    day: r.get(0)?,
                    prompt_tokens: r.get(1)?,
                    completion_tokens: r.get(2)?,
                    cached_tokens: r.get(3)?,
                    total_tokens: r.get(4)?,
                    cost_usd: r.get(5)?,
                    record_count: r.get(6)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 维度分布桶
#[derive(Debug, Serialize)]
pub struct DistBucket {
    pub key: String,
    pub total_tokens: i64,
    pub cost_usd: Option<f64>,
    pub record_count: i64,
}

/// 按指定维度分组统计。dimension ∈ model_name | provider_code | project | session_id
pub fn distribution(
    conn: &Connection,
    f: &RecordFilter,
    dimension: &str,
    limit: i64,
) -> Result<Vec<DistBucket>> {
    let dim = match dimension {
        "provider_code" | "model_name" | "project" | "session_id" => dimension,
        _ => "model_name",
    };
    let (where_sql, params) = filter_sql(f);
    let limit = limit.clamp(1, 100);
    let sql = format!(
        "SELECT COALESCE(NULLIF({dim},''),'(未标注)') AS key,
                COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0),
                SUM(cost_usd),
                COUNT(*)
         FROM usage_record {where_sql}
         GROUP BY key ORDER BY 2 DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok(DistBucket {
                    key: r.get(0)?,
                    total_tokens: r.get(1)?,
                    cost_usd: r.get(2)?,
                    record_count: r.get(3)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 供下拉框用的模型清单(出现过 + 价格库)
pub fn known_models(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT model_name FROM usage_record
         WHERE model_name IS NOT NULL AND model_name <> ''
         UNION
         SELECT name FROM model
         ORDER BY 1",
    )?;
    let out = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_lists_builtin_models() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let models = known_models(&conn).unwrap();
        assert!(models.contains(&"deepseek-v4-flash".to_string()));
        assert!(models.contains(&"gpt-5.6-sol".to_string()));
    }

    #[test]
    fn overview_empty_db_is_zero() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let o = overview(&conn, &RecordFilter::default()).unwrap();
        assert_eq!(o.record_count, 0);
        assert_eq!(o.total_tokens, 0);
        assert!(o.cost_usd.is_none());
    }
}
