//! 端到端集成测试: 临时文件库上执行 migrate → CSV 导入 → 统计 → 导出。

use std::collections::HashMap;

use token_usage_tracker_lib::db;
use token_usage_tracker_lib::domain::export as de;
use token_usage_tracker_lib::domain::import::{import_csv, ColumnMapping};
use token_usage_tracker_lib::domain::records::{self, RecordFilter};
use token_usage_tracker_lib::domain::stats;

fn setup_db() -> (tempfile::NamedTempFile, rusqlite::Connection) {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let conn = db::open(file.path()).expect("open");
    db::migrate(&conn).expect("migrate");
    (file, conn)
}

fn write_csv(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).expect("write csv");
}

#[test]
fn full_import_stats_export_flow() {
    let (_file, conn) = setup_db();

    // 构造 2 天、2 模型、2 厂商的样例数据
    let csv = "\
recorded_at,model,provider,prompt_tokens,completion_tokens,cached_tokens,cost
2026-09-01 08:00:00,deepseek-v4-flash,deepseek,1000000,1000000,200000,1.76
2026-09-01 09:00:00,deepseek-v4-flash,deepseek,1000000,500000,0,
2026-09-02 10:00:00,gpt-5.6-sol,openai,200000,100000,0,
2026-09-02 11:00:00,claude-opus-5,anthropic,100000,50000,0,
2026-09-01 08:00:00,deepseek-v4-flash,deepseek,1000000,1000000,200000,1.76
";
    let csv_path = _file.path().with_extension("csv");
    write_csv(&csv_path, csv);

    let mut map = HashMap::new();
    map.insert("recorded_at".to_string(), "recorded_at".to_string());
    map.insert("model".to_string(), "model_name".to_string());
    map.insert("provider".to_string(), "provider_code".to_string());
    map.insert("prompt_tokens".to_string(), "prompt_tokens".to_string());
    map.insert("completion_tokens".to_string(), "completion_tokens".to_string());
    map.insert("cached_tokens".to_string(), "cached_tokens".to_string());
    map.insert("cost".to_string(), "cost_usd".to_string());
    let mapping = ColumnMapping { map, batch_source: Some("sample".to_string()) };

    let result = import_csv(&conn, csv_path.to_str().unwrap(), &mapping, "sample.csv").expect("import");
    // 5 行中第 5 行与第 1 行 request_id 都为空 → 不去重; 全部成功
    assert_eq!(result.total_rows, 5, "total rows");
    assert_eq!(result.ok_rows, 5, "ok rows");
    assert_eq!(result.failed_rows, 0, "failed rows");
    assert_eq!(result.errors.len(), 0, "no errors: {:?}", result.errors);

    // 统计: 2 天、3 模型、3 厂商
    let o = stats::overview(&conn, &RecordFilter::default()).expect("overview");
    assert_eq!(o.record_count, 5);
    assert_eq!(o.day_count, 2);
    assert_eq!(o.model_count, 3);
    assert_eq!(o.provider_count, 3);
    // token: 4M+1.5M+0.3M+0.15M(+重复行1M? ) 第5行重复成本一样
    // 总计 prompt: 1M+1M+200k+100k+1M = 3.3M; completion: 1M+500k+100k+50k+1M=2.65M
    assert_eq!(o.prompt_tokens, 3_300_000);
    assert_eq!(o.completion_tokens, 2_650_000);

    // 成本: 自带 1.76 两行; deepseek 第二行 1M prompt+500k out 按内置价 0.44+0.66(注: 0.66=1.32/2? 不对)
    // deepseek-v4-flash price: in 0.44, out 1.32, cached 0.014
    // 行2: prompt 1M(无缓存), comp 500k → 0.44 + 1.32*0.5 = 1.10
    // 行3 gpt-5.6-sol: in 5, out 30 → 200k*5/1M + 100k*30/1M = 1.0+3.0 = 4.0
    // 行4 claude-opus-5: in 5 out 25 → 0.1*5? wait 100k*5/1M=0.5; 50k*25/1M=1.25 → 1.75
    let total_cost = o.cost_usd.expect("cost");
    let expected = 1.76 + 1.76 + 1.10 + 4.0 + 1.75;
    assert!((total_cost - expected).abs() < 1e-6, "cost {total_cost} vs {expected}");

    // 趋势: 按日两条
    let trend = stats::trend(&conn, &RecordFilter::default()).expect("trend");
    assert_eq!(trend.len(), 2);

    // 分布
    let by_model = stats::distribution(&conn, &RecordFilter::default(), "model_name", 10).expect("dist");
    assert_eq!(by_model.len(), 3);

    // 导出 CSV 应含表头与 5 行
    let payload = de::to_csv(&conn, &RecordFilter::default()).expect("export csv");
    assert_eq!(payload.rows, 5);
    assert!(payload.content.contains("recorded_at"));
    assert!(payload.content.starts_with('\u{feff}')); // BOM

    // 导出 JSON
    let jp = de::to_json(&conn, &RecordFilter::default()).expect("export json");
    assert_eq!(jp.rows, 5);
    let parsed: serde_json::Value = serde_json::from_str(&jp.content).expect("valid json");
    assert_eq!(parsed.as_array().unwrap().len(), 5);
}

#[test]
fn import_dedups_by_request_id() {
    let (_file, conn) = setup_db();
    let csv = "\
recorded_at,model,provider,request_id,prompt_tokens,completion_tokens
2026-09-01 08:00:00,deepseek-v4-flash,deepseek,req-1,1000,500
2026-09-01 09:00:00,deepseek-v4-flash,deepseek,req-1,1000,500
2026-09-01 10:00:00,deepseek-v4-flash,deepseek,req-2,2000,1000
";
    let csv_path = _file.path().with_extension("csv2");
    write_csv(&csv_path, csv);
    let mut map = HashMap::new();
    map.insert("recorded_at".to_string(), "recorded_at".to_string());
    map.insert("model".to_string(), "model_name".to_string());
    map.insert("provider".to_string(), "provider_code".to_string());
    map.insert("request_id".to_string(), "request_id".to_string());
    map.insert("prompt_tokens".to_string(), "prompt_tokens".to_string());
    map.insert("completion_tokens".to_string(), "completion_tokens".to_string());
    let r = import_csv(
        &conn,
        csv_path.to_str().unwrap(),
        &ColumnMapping { map, batch_source: None },
        "d.csv",
    )
    .expect("import");
    assert_eq!(r.ok_rows, 2, "second req-1 skipped");
    assert_eq!(r.skipped_rows, 1);
    assert_eq!(records::count(&conn, &RecordFilter::default()).unwrap(), 2);
}
