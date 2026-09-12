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

/// 趋势序列中的一个点(按小时, 今日视图用)
#[derive(Debug, Clone, Serialize)]
pub struct HourTrendPoint {
    pub hour: String, // 2026-09-08T14
    pub total_tokens: i64,
    pub cost_usd: Option<f64>,
    pub record_count: i64,
}

/// 按小时(本地时区)聚合, 返回某日 00~23 的小时序列(无数据补零)。
pub fn hourly_trend(conn: &Connection, f: &RecordFilter) -> Result<Vec<HourTrendPoint>> {
    let (where_sql, params) = filter_sql(f);
    let sql = format!(
        "SELECT strftime('%Y-%m-%dT%H', recorded_at, 'localtime') AS hour,
                COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0),
                SUM(cost_usd),
                COUNT(*)
         FROM usage_record {where_sql}
         GROUP BY hour ORDER BY hour ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok(HourTrendPoint {
                    hour: r.get(0)?,
                    total_tokens: r.get(1)?,
                    cost_usd: r.get(2)?,
                    record_count: r.get(3)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;

    // 若只查了一天, 则补成完整 24 小时(缺的填 0), 便于今日曲线连续
    if out.iter().all(|p| p.hour.len() >= 13) {
        let days: std::collections::BTreeSet<String> =
            out.iter().map(|p| p.hour[..10].to_string()).collect();
        if days.len() == 1 {
            let day = days.iter().next().cloned().unwrap_or_default();
            let filled: Vec<HourTrendPoint> = (0..24)
                .map(|h| {
                    let key = format!("{day}T{:02}", h);
                    out.iter()
                        .find(|p| p.hour == key)
                        .cloned()
                        .unwrap_or(HourTrendPoint {
                            hour: key,
                            total_tokens: 0,
                            cost_usd: None,
                            record_count: 0,
                        })
                })
                .collect();
            return Ok(filled);
        }
    }
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
        "provider_code" | "model_name" | "project" | "session_id" | "source" => dimension,
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

/// 按采集来源(软件)拆分用量, 供看板来源标签筛选使用。
///
/// 注意: 内部会清掉 `f.source` 再聚合 —— 否则选中某个来源后,
/// 其它标签的计数会全部归零, 标签栏就没法在"选中态"下继续显示各家用量。
/// 日期范围等其它条件仍然生效。
pub fn source_breakdown(conn: &Connection, f: &RecordFilter) -> Result<Vec<DistBucket>> {
    let mut unselected = f.clone();
    unselected.source = None;
    distribution(conn, &unselected, "source", 50)
}

/// 全部历史出现过的采集来源(不限日期)。
///
/// 供看板来源标签栏使用: 标签必须始终完整。若标签列表受日期范围约束,
/// 跨天之后"今日"没有数据的软件会连标签一起消失, 用户会误以为数据丢失。
pub fn all_sources(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT source FROM usage_record
         WHERE source IS NOT NULL AND source <> ''
         ORDER BY 1",
    )?;
    let out = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 供下拉框用的模型清单(出现过 + 价格库)
pub fn known_models(conn: &Connection) -> Result<Vec<String>> {    let mut stmt = conn.prepare(
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

    #[test]
    fn hourly_trend_fills_24h_for_single_day() {        use crate::domain::records::{insert, NewRecord};
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let rec = NewRecord {
            recorded_at: "2026-09-08 10:30:00".into(),
            source: "manual".into(),
            model_name: Some("deepseek-v4-flash".into()),
            prompt_tokens: Some(10),
            completion_tokens: Some(10),
            ..Default::default()
        };
        insert(&conn, &rec, None).unwrap();

        let f = RecordFilter { from: Some("2026-09-08".into()), to: Some("2026-09-08".into()), ..Default::default() };
        let points = hourly_trend(&conn, &f).unwrap();
        assert_eq!(points.len(), 24, "今日应补满 24 小时");
        // 记录落在某小时(受本机时区影响, 不断言具体小时), 其余 23 小时为 0
        let nonzero = points.iter().filter(|p| p.total_tokens > 0).count();
        assert_eq!(nonzero, 1, "应恰有一个小时含数据");
        let hit = points.iter().find(|p| p.total_tokens == 20).expect("20 tokens 的点存在");
        assert!(hit.hour.starts_with("2026-09-08T"));
        let empty = points.iter().find(|p| p.total_tokens == 0).unwrap();
        assert_eq!(empty.cost_usd, None);
    }

    /// 播种三条不同来源/日期的记录
    fn seed_sources(conn: &Connection) {
        use crate::domain::records::{insert, NewRecord};
        let rows = [
            ("dsh", "2026-09-08 10:00:00"),
            ("claude_code", "2026-09-08 11:00:00"),
            ("claude_code", "2026-09-09 11:00:00"),
        ];
        for (src, at) in rows {
            let rec = NewRecord {
                recorded_at: at.into(),
                source: src.into(),
                model_name: Some("m1".into()),
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                ..Default::default()
            };
            insert(conn, &rec, None).unwrap();
        }
    }

    #[test]
    fn filter_by_source_selects_single_software() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        seed_sources(&conn);

        let all = overview(&conn, &RecordFilter::default()).unwrap();
        assert_eq!(all.record_count, 3);

        let f = RecordFilter { source: Some("claude_code".into()), ..Default::default() };
        let only_cc = overview(&conn, &f).unwrap();
        assert_eq!(only_cc.record_count, 2, "应只剩 Claude Code 的 2 条");
        assert_eq!(only_cc.total_tokens, 300, "2 条 × (100+50)");

        // 空字符串视为"不筛选"(前端 '全部' 标签)
        let f_empty = RecordFilter { source: Some(String::new()), ..Default::default() };
        assert_eq!(overview(&conn, &f_empty).unwrap().record_count, 3);
    }

    #[test]
    fn source_breakdown_ignores_source_filter_but_keeps_date() {        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        seed_sources(&conn);

        // 选中 dsh 的同时请求标签栏数据: 仍应看到两个来源(否则标签栏会塌成一项)
        let f = RecordFilter {
            from: Some("2026-09-08".into()),
            to: Some("2026-09-08".into()),
            source: Some("dsh".into()),
            ..Default::default()
        };
        let buckets = source_breakdown(&conn, &f).unwrap();
        let mut keys: Vec<String> = buckets.iter().map(|b| b.key.clone()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["claude_code".to_string(), "dsh".to_string()],
            "应忽略 source 自身, 但受日期范围约束(9-09 的记录被排除)"
        );
        // 每个来源各 1 条(9-08 当天)
        for b in &buckets {
            assert_eq!(b.record_count, 1, "来源 {} 在 9-08 只有 1 条", b.key);
            assert_eq!(b.total_tokens, 150);
        }
    }

    /// 标签栏的来源清单必须与日期范围无关。
    ///
    /// 回归背景: 标签列表若只用"当前时间段有数据的来源", 跨天之后
    /// (如 00:08 时默认「今日」已翻到新的一天) 没有新数据的软件会连标签
    /// 一起消失, 用户会以为数据丢了 —— 实际数据仍在库里。
    #[test]
    fn all_sources_is_range_independent() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        seed_sources(&conn); // dsh(9-08), claude_code(9-08), claude_code(9-09)

        assert_eq!(
            all_sources(&conn).unwrap(),
            vec!["claude_code".to_string(), "dsh".to_string()]
        );

        // 限定到一个完全没有数据的日期: 面板数据为空, 但标签清单不缩水
        let empty_day = RecordFilter {
            from: Some("2026-09-30".into()),
            to: Some("2026-09-30".into()),
            ..Default::default()
        };
        assert!(
            source_breakdown(&conn, &empty_day).unwrap().is_empty(),
            "该日期确实没有数据"
        );
        assert_eq!(
            all_sources(&conn).unwrap().len(),
            2,
            "标签清单不应随日期范围缩水"
        );
    }
}
