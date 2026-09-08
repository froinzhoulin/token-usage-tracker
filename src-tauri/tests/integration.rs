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

#[test]
fn time_normalization_and_local_day_filter() {
    use token_usage_tracker_lib::domain::records::{self, NewRecord, RecordFilter};
    let (_file, conn) = setup_db();

    let insert = |recorded_at: &str, model: &str| {
        let rec = NewRecord {
            recorded_at: recorded_at.into(),
            source: "manual".into(),
            model_name: Some(model.into()),
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            ..Default::default()
        };
        records::insert(&conn, &rec, None).unwrap();
    };

    // 入库格式应统一为规范 ISO UTC(固定宽度, 字符串比较=时间比较)
    insert("2026-09-01 08:00:00", "m-a"); // 空格格式 → 规范化
    insert("2026-09-01T08:00:00Z", "m-b"); // 带 T/Z → 规范化
    insert("2026-09-08 12:34:56", "m-c"); // 今天(未来某日)

    let stored: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT recorded_at FROM usage_record ORDER BY id")
            .unwrap();
        let v = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        v
    };
    for s in &stored {
        assert!(s.contains('T') && s.ends_with('Z'), "规范化失败: {s}");
    }
    assert_eq!(stored[0], "2026-09-01T08:00:00.000Z");
    assert_eq!(stored[1], "2026-09-01T08:00:00.000Z");

    // normalize 直接单测
    assert_eq!(
        records::normalize_recorded_at("2026-09-01 08:00:00"),
        "2026-09-01T08:00:00.000Z"
    );
    assert_eq!(
        records::normalize_recorded_at("2026-09-01T08:00:00Z"),
        "2026-09-01T08:00:00.000Z"
    );
    assert_eq!(
        records::normalize_recorded_at("2026-09-01"),
        "2026-09-01T00:00:00.000Z"
    );

    // 过滤: from/to 是本地时区日期串, 后端换算为 UTC 边界。
    // 08:00 UTC 在任何常见时区(UTC-12~+14)都落在本地 09-01, 故本地日 09-01 恰好两条。
    let f = RecordFilter {
        from: Some("2026-09-01".into()),
        to: Some("2026-09-01".into()),
        ..Default::default()
    };
    assert_eq!(records::count(&conn, &f).unwrap(), 2, "day 09-01 count");

    let f2 = RecordFilter {
        from: Some("2026-09-08".into()),
        to: Some("2026-09-08".into()),
        ..Default::default()
    };
    assert_eq!(records::count(&conn, &f2).unwrap(), 1, "day 09-08 count");

    let f3 = RecordFilter {
        from: Some("2026-09-02".into()),
        to: Some("2026-09-07".into()),
        ..Default::default()
    };
    assert_eq!(records::count(&conn, &f3).unwrap(), 0, "no records in range");
}

/// 验收标准 #1: 10 万行记录导入且看板统计 < 1s。
/// 手动运行: cargo test --release --test integration perf_100k -- --ignored --nocapture
#[test]
#[ignore]
fn perf_100k_rows() {
    let (_file, conn) = setup_db();

    // 流式生成 10 万行 CSV
    let csv_path = _file.path().with_extension("perf.csv");
    {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(&csv_path).unwrap());
        writeln!(
            w,
            "recorded_at,model,provider,request_id,prompt_tokens,completion_tokens,cost_usd"
        )
        .unwrap();
        for i in 0..100_000i64 {
            let model = if i % 3 == 0 { "deepseek-v4-flash" } else if i % 3 == 1 { "gpt-5.6-sol" } else { "claude-opus-5" };
            let provider = if i % 3 == 0 { "deepseek" } else if i % 3 == 1 { "openai" } else { "anthropic" };
            writeln!(
                w,
                "2026-0{:02}-{:02} 08:00:00,{model},{provider},perf-req-{i},{},200,,",
                (i % 9) + 1,
                (i % 28) + 1,
                500 + (i % 5000) * 100
            )
            .unwrap();
        }
    }

    let mut map = HashMap::new();
    for (src, dst) in [
        ("recorded_at", "recorded_at"),
        ("model", "model_name"),
        ("provider", "provider_code"),
        ("request_id", "request_id"),
        ("prompt_tokens", "prompt_tokens"),
        ("completion_tokens", "completion_tokens"),
    ] {
        map.insert(src.to_string(), dst.to_string());
    }

    let t0 = std::time::Instant::now();
    let r = import_csv(
        &conn,
        csv_path.to_str().unwrap(),
        &ColumnMapping { map, batch_source: None },
        "perf.csv",
    )
    .expect("import 100k");
    let import_ms = t0.elapsed().as_millis();
    assert_eq!(r.ok_rows, 100_000, "all imported, errs: {:?}", r.errors.first());

    let t1 = std::time::Instant::now();
    let o = stats::overview(&conn, &RecordFilter::default()).unwrap();
    let overview_ms = t1.elapsed().as_millis();
    assert_eq!(o.record_count, 100_000);

    let t2 = std::time::Instant::now();
    let trend = stats::trend(&conn, &RecordFilter::default()).unwrap();
    let trend_ms = t2.elapsed().as_millis();
    assert!(trend.len() >= 28);

    // 看板核心查询合计应远低于 1s(release 模式断言 <1000ms)
    let total_ms = overview_ms + trend_ms;
    println!(
        "perf: import={import_ms}ms overview={overview_ms}ms trend={trend_ms}ms total-dashboard={total_ms}ms"
    );
    assert!(
        total_ms < 1000,
        "dashboard queries too slow: {total_ms}ms"
    );
}
