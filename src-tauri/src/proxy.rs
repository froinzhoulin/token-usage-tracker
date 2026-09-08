//! 本地透明代理(v0.4 核心): "自动检测"入口。
//!
//! 用户把程序中 OpenAI 兼容的 base_url 从 `https://api.deepseek.com`
//! 指向本工具监听地址 `http://127.0.0.1:<port>`, 其余代码不动。
//! 本模块把 /chat/completions、/v1/chat/completions 等请求原样转发到
//! 上游, 同时解析(流式/非流式)响应中的 usage 自动入库。
//!
//! - API Key 完全透传: 不读取、不落盘、不保存;
//! - 流式请求自动注入 stream_options.include_usage=true 以便拿到 usage;
//! - 转发过程中不持有 DB 锁(可能耗时数分钟), 仅开始读配置/结束入库时短锁;
//! - 流式响应通过 io::pipe 逐块实时回传客户端(打字机体验不破坏)。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::domain::records::NewRecord;

static HTTP_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn client() -> &'static reqwest::blocking::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("http client build")
    })
}

/// 代理目标路径(OpenAI 兼容)。工具自身 API 不代理。
pub fn is_proxy_path(method: &str, path: &str) -> bool {
    if path == "/api/v1/usage" || path == "/api/v1/ping" || path == "/" {
        return false;
    }
    // OpenAI 兼容端点: POST 以 /chat/completions、/completions、/responses 结尾,
    // 或 GET /models 等以 /v1/ 前缀、/models 结尾的地址
    (method == "POST"
        && (path.ends_with("/chat/completions")
            || path.ends_with("/completions")
            || path.ends_with("/responses")))
        || path.ends_with("/models")
        || (path.starts_with("/v1/") && !path.contains("usage") && !path.contains("ping"))
}

/// 上游地址(从 kv_settings 读取, 默认 DeepSeek)
fn upstream_base(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM kv_settings WHERE key='collector_upstream'",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|s| !s.trim().is_empty())
    .unwrap_or_else(|| "https://api.deepseek.com".to_string())
}

/// 从请求体取 model 与 stream 标志; 流式时注入 stream_options.include_usage。
fn inspect_body(body: &[u8]) -> Result<(Option<String>, bool, Vec<u8>), String> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|e| format!("请求体不是合法 JSON: {e}"))?;
    let model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    let stream = parsed
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let mut patched = parsed;
    if stream {
        let opts = patched
            .get_mut("stream_options")
            .cloned()
            .unwrap_or_else(|| json!({"include_usage": true}));
        let mut opts = opts;
        if opts.get("include_usage").and_then(|v| v.as_bool()) != Some(true) {
            opts["include_usage"] = json!(true);
        }
        patched["stream_options"] = opts;
    }
    let patched = serde_json::to_vec(&patched).map_err(|e| format!("请求体重编码失败: {e}"))?;
    Ok((model, stream, patched))
}

/// 从响应的顶层/usage JSON 中提取 token 计数(兼容 DeepSeek 与 OpenAI 结构)
#[derive(Debug, Default)]
pub struct ParsedUsage {
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
}

fn parse_usage_json(v: &Value) -> Option<ParsedUsage> {
    let usage = v.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|x| x.as_i64())
        .or_else(|| usage.get("input_tokens").and_then(|x| x.as_i64()));
    let completion = usage
        .get("completion_tokens")
        .and_then(|x| x.as_i64())
        .or_else(|| usage.get("output_tokens").and_then(|x| x.as_i64()));
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    // DeepSeek: prompt_cache_hit_tokens; OpenAI: prompt_tokens_details.cached_tokens
    let cached = usage
        .get("prompt_cache_hit_tokens")
        .and_then(|x| x.as_i64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|x| x.as_i64())
        });
    Some(ParsedUsage {
        model: v
            .get("model")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string()),
        request_id: v
            .get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string()),
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_tokens: cached,
    })
}

/// 将解析出的 usage 入库(source=proxy)。request_id 用于去重。
fn record_usage(conn: &Connection, u: &ParsedUsage, fallback_model: Option<&str>) {
    let model = u.model.as_deref().or(fallback_model);
    if model.is_none() || (u.prompt_tokens.is_none() && u.completion_tokens.is_none()) {
        return;
    }
    let rec = NewRecord {
        recorded_at: chrono::Utc::now().to_rfc3339(),
        source: "proxy".into(),
        provider_code: Some("deepseek".into()),
        model_name: model.map(|s| s.to_string()),
        session_id: None,
        request_id: u.request_id.clone(),
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        cached_tokens: u.cached_tokens,
        cost_usd: None,
        cost_source: None,
        project: None,
        tags: None,
        note: Some("auto via local proxy".into()),
    };
    let _ = crate::domain::records::insert(conn, &rec, None);
}

fn pick_forward_headers(
    req_headers: &[(String, String)],
    builder: reqwest::blocking::RequestBuilder,
) -> reqwest::blocking::RequestBuilder {
    let mut b = builder;
    for (k, v) in req_headers {
        let lk = k.to_ascii_lowercase();
        if lk == "authorization" || lk == "content-type" || lk == "accept" {
            if let Ok(hk) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(hv) = reqwest::header::HeaderValue::from_str(v) {
                    b = b.header(hk, hv);
                }
            }
        }
    }
    b
}

/// 非流式/整体缓冲式转发(亦作为测试基座): 返回 (status, headers, body)。
pub fn handle_proxy(
    conn: &Arc<Mutex<Connection>>,
    method: &str,
    path: &str,
    req_headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    // 1) 短锁: 读上游地址(不含 key, key 来自客户端透传)
    let upstream = {
        let g = conn.lock().map_err(|e| e.to_string())?;
        upstream_base(&g)
    };
    let url = format!("{}{}", upstream.trim_end_matches('/'), path);
    let (fallback_model, stream, patched) = inspect_body(body)?;

    // 2) 无锁转发
    let mut req = client().request(
        reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| format!("非法方法 {method}"))?,
        &url,
    );
    req = pick_forward_headers(req_headers, req);
    let resp = req
        .body(patched)
        .send()
        .map_err(|e| format!("转发到 {upstream} 失败: {e}"))?;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let bytes = resp.bytes().map_err(|e| format!("读取上游响应失败: {e}"))?;

    // 3) 解析 usage(有锁更好, 但解析本身不需要 db)
    if status < 400 {
        if stream {
            scan_sse_usage(&bytes, |u| {
                let g = conn.lock().ok();
                if let Some(g) = g {
                    record_usage(&g, u, fallback_model.as_deref());
                }
            });
        } else if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(u) = parse_usage_json(&v) {
                let g = conn.lock().map_err(|e| e.to_string())?;
                record_usage(&g, &u, fallback_model.as_deref());
            }
        }
    }

    Ok((
        status,
        vec![
            ("content-type".to_string(), ct),
            ("access-control-allow-origin".to_string(), "*".to_string()),
        ],
        bytes.to_vec(),
    ))
}

/// 解析 SSE 文本中的 usage 事件并回调。
fn scan_sse_usage<F: FnMut(&ParsedUsage)>(sse: &[u8], mut on_usage: F) {
    let text = String::from_utf8_lossy(sse);
    let mut pending = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !pending.is_empty() {
                if let Some(data) = pending.trim().strip_prefix("data:") {
                    let data = data.trim();
                    if data != "[DONE]" {
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            if let Some(u) = parse_usage_json(&v) {
                                on_usage(&u);
                            }
                        }
                    }
                }
                pending.clear();
            }
            continue;
        }
        pending.push_str(line);
    }
    if !pending.is_empty() {
        if let Some(data) = pending.trim().strip_prefix("data:") {
            let data = data.trim();
            if data != "[DONE]" {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(u) = parse_usage_json(&v) {
                        on_usage(&u);
                    }
                }
            }
        }
    }
}

/// 流式实时转发: 上游响应逐块经 pipe 回传客户端, 同一线程边转发边解析 usage。
/// 返回 (status, [(k,v)], pipe_reader) 供 tiny_http Response::new 使用。
pub fn handle_proxy_streaming(
    conn_arc: Arc<Mutex<Connection>>,
    path: &str,
    req_headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, Vec<(String, String)>, std::io::PipeReader), String> {
    let (fallback_model, _, patched) = inspect_body(body)?;

    let upstream = {
        let g = conn_arc.lock().map_err(|e| e.to_string())?;
        upstream_base(&g)
    };
    let url = format!("{}{}", upstream.trim_end_matches('/'), path);

    let mut req = client().request(reqwest::Method::POST, &url);
    req = pick_forward_headers(req_headers, req);
    let mut resp = req
        .body(patched)
        .send()
        .map_err(|e| format!("转发到 {upstream} 失败: {e}"))?;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();

    let (reader, writer) = std::io::pipe().map_err(|e| format!("pipe 创建失败: {e}"))?;
    let mut writer = writer;

    // 转发线程: 读上游 → 写 pipe; 收集 data 行, 找到 usage 事件即入库(短锁)
    let fm = fallback_model.clone();
    let conn_t = Arc::clone(&conn_arc);
    std::thread::Builder::new()
        .name("proxy-forward".into())
        .spawn(move || {
            let mut buf = [0u8; 16384];
            let mut line_acc: Vec<u8> = Vec::with_capacity(32 * 1024);
            let mut recorded = false;
            loop {
                match resp.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer.write_all(&buf[..n]).is_err() {
                            break; // 客户端断开
                        }
                        if !recorded {
                            line_acc.extend_from_slice(&buf[..n]);
                            // 提取完整 data 行
                            while let Some(pos) = line_acc.iter().position(|b| *b == b'\n') {
                                let line: Vec<u8> = line_acc.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line);
                                if let Some(data) = line.trim().strip_prefix("data:") {
                                    let data = data.trim();
                                    if data != "[DONE]" {
                                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                                            if let Some(u) = parse_usage_json(&v) {
                                                if let Ok(g) = conn_t.lock() {
                                                    record_usage(&g, &u, fm.as_deref());
                                                    recorded = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if line_acc.len() > 512 * 1024 {
                                line_acc.clear();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            drop(writer); // 关闭 → 客户端 EOF
        })
        .map_err(|e| format!("转发线程启动失败: {e}"))?;

    Ok((
        status,
        vec![
            ("content-type".to_string(), ct),
            ("access-control-allow-origin".to_string(), "*".to_string()),
        ],
        reader,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mem_conn() -> (tempfile::TempDir, Arc<Mutex<Connection>>) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        let conn = crate::db::open(&db_path).unwrap();
        crate::db::migrate(&conn).unwrap();
        (dir, Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn path_detection() {
        assert!(is_proxy_path("POST", "/v1/chat/completions"));
        assert!(is_proxy_path("POST", "/chat/completions"));
        assert!(is_proxy_path("GET", "/v1/models"));
        assert!(!is_proxy_path("GET", "/api/v1/usage"));
        assert!(!is_proxy_path("POST", "/api/v1/usage"));
        assert!(!is_proxy_path("GET", "/api/v1/ping"));
    }

    #[test]
    fn injects_include_usage_for_stream() {
        let body = br#"{"model":"deepseek-chat","messages":[],"stream":true}"#;
        let (_, stream, patched) = inspect_body(body).unwrap();
        assert!(stream);
        let v: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], json!(true));
        // 非流式不注入
        let body2 = br#"{"model":"m","messages":[]}"#;
        let (_, s2, p2) = inspect_body(body2).unwrap();
        assert!(!s2);
        let v2: Value = serde_json::from_slice(&p2).unwrap();
        assert!(v2.get("stream_options").is_none());
    }

    #[test]
    fn parses_deepseek_usage() {
        let v: Value = serde_json::from_str(
            r#"{"id":"chatcmpl-x","model":"deepseek-chat",
                "usage":{"prompt_tokens":120,"completion_tokens":34,
                         "prompt_cache_hit_tokens":50,"prompt_cache_miss_tokens":70,
                         "total_tokens":154}}"#,
        )
        .unwrap();
        let u = parse_usage_json(&v).expect("usage present");
        assert_eq!(u.prompt_tokens, Some(120));
        assert_eq!(u.completion_tokens, Some(34));
        assert_eq!(u.cached_tokens, Some(50));
    }

    #[test]
    fn parses_openai_style_cached() {
        let v: Value = serde_json::from_str(
            r#"{"model":"gpt-x","usage":{"prompt_tokens":10,"completion_tokens":5,
                "prompt_tokens_details":{"cached_tokens":7}}}"#,
        )
        .unwrap();
        let u = parse_usage_json(&v).unwrap();
        assert_eq!(u.cached_tokens, Some(7));
    }

    #[test]
    fn sse_scanner_finds_usage() {
        let (_dir, conn_arc) = mem_conn();
        let g = conn_arc.lock().unwrap();
        let mut found = 0;
        scan_sse_usage(
            br#"data: {"id":"a","choices":[{"delta":{"content":"hi"}}]}

data: {"id":"a","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}

data: [DONE]
"#,
            |_| found += 1,
        );
        drop(g);
        assert_eq!(found, 1, "只应命中 usage 事件一次");
    }

    #[test]
    fn nonstream_proxy_records_usage_with_fake_upstream() {
        let (_dir, conn_arc) = mem_conn();

        // 假上游
        let fake = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let fake_port = fake.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || loop {
            let Ok(mut req) = fake.recv() else { break };
            let response = tiny_http::Response::from_string(
                r#"{"id":"fake-1","model":"deepseek-chat","choices":[{"index":0}],
                    "usage":{"prompt_tokens":222,"completion_tokens":33,
                             "prompt_cache_hit_tokens":11}}"#,
            )
            .with_status_code(200)
            .with_header(
                tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                    .unwrap(),
            );
            let _ = req.respond(response);
        });

        // 上游指向假服务
        {
            let g = conn_arc.lock().unwrap();
            g.execute(
                "INSERT OR REPLACE INTO kv_settings(key,value) VALUES('collector_upstream', ?1)",
                rusqlite::params![format!("http://127.0.0.1:{fake_port}")],
            )
            .unwrap();
        }

        let body = br#"{"model":"deepseek-chat","messages":[{"role":"user","content":"hi"}]}"#;
        let (status, headers, resp_body) = handle_proxy(
            &conn_arc,
            "POST",
            "/chat/completions",
            &[("authorization".into(), "Bearer sk-test".into())],
            body,
        )
        .expect("proxy ok");
        assert_eq!(status, 200);
        assert!(headers.iter().any(|(k, _)| k == "content-type"));
        let parsed: Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(parsed["model"], "deepseek-chat");

        let g = conn_arc.lock().unwrap();
        let (n, p, c): (i64, i64, i64) = g
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0)
                 FROM usage_record WHERE source='proxy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "应入库一条 proxy 记录");
        assert_eq!(p, 222);
        assert_eq!(c, 33);
    }
}
