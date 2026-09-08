//! 本地用量收集端点(v0.2 核心入口)。
//!
//! 应用启动后在 127.0.0.1:<collector_port> 监听一个极小 HTTP 服务,
//! 本机程序/脚本调用国产大模型后, 把用量事件 POST 到这里即自动入库:
//!
//! ```text
//! POST /api/v1/usage
//! Content-Type: application/json
//!
//! {
//!   "model_name": "deepseek-chat",        // 必填
//!   "provider_code": "deepseek",          // 可选, 缺省按模型名猜/留空
//!   "prompt_tokens": 1234,                // 推荐给全
//!   "completion_tokens": 567,
//!   "cached_tokens": 0,                   // 可选
//!   "session_id": "s-1",                  // 可选
//!   "request_id": "r-1",                  // 可选, 提供则去重
//!   "cost_cny": 0.012,                    // 可选, 官方账单金额(人民币)则优先采用
//!   "recorded_at": "2026-09-07T12:00:00Z" // 可选, 缺省当前时间
//! }
//! ```
//!
//! 另有 `GET /api/v1/ping` 用于连通性测试。
//! 服务只绑定回环地址, 外部不可达。

use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;

/// collector 启动结果: 端口或失败原因(供前端展示)。
#[derive(Clone, serde::Serialize)]
pub struct CollectorInfo {
    pub port: u16,
    pub started: bool,
    pub error: Option<String>,
}

/// 用量上报请求体(字段较 NewRecord 更宽松, 面向外部调用方)。
#[derive(Debug, Default, Deserialize)]
pub struct UsageReport {
    pub model_name: Option<String>,
    pub model: Option<String>,
    pub provider_code: Option<String>,
    pub provider: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    /// 官方账单金额(人民币元); 提供则优先采用, 否则按单价库估算
    pub cost_cny: Option<f64>,
    pub cost_usd: Option<f64>,
    pub note: Option<String>,
    pub recorded_at: Option<String>,
}

/// 绑定并启动收集服务(阻塞线程循环)。失败返回 Err(原因)。
pub fn start(
    conn: Arc<Mutex<Connection>>,
    port: u16,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Result<CollectorInfo, String> {
    let addr = format!("127.0.0.1:{port}");
    let server =
        tiny_http::Server::http(&addr).map_err(|e| format!("端口 {port} 绑定失败: {e}"))?;
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);

    std::thread::Builder::new()
        .name("usage-collector".into())
        .spawn(move || {
            // 代理转发可能长时间阻塞, 每个请求独立线程处理
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match server.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(Some(request)) => {
                        let conn = Arc::clone(&conn);
                        std::thread::Builder::new()
                            .name("collector-req".into())
                            .spawn(move || handle_request(&conn, request))
                            .map_err(|e| eprintln!("[collector] spawn failed: {e}"))
                            .ok();
                    }
                    Ok(None) => continue,
                    Err(_) => continue,
                }
            }
        })
        .map_err(|e| format!("收集线程启动失败: {e}"))?;

    Ok(CollectorInfo { port: bound_port, started: true, error: None })
}

fn handle_request(conn: &Arc<Mutex<Connection>>, mut request: tiny_http::Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();

    // ---- 透明代理路径: 转发到真实上游, 自动解析 usage 入库 ----
    if crate::proxy::is_proxy_path(method.as_str(), &path) {
        // 读取请求体(上限 8MB)
        let mut body = Vec::new();
        if let Err(e) = request
            .as_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut body)
        {
            let resp = tiny_http::Response::from_string(format!(
                "{{\"error\":\"读取请求体失败: {e}\"}}"
            ))
            .with_status_code(400);
            let _ = request.respond(resp);
            return;
        }
        // 判断是否流式请求(简单看 body 中 stream 字段)
        let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
            .unwrap_or(false);

        let headers = request
            .headers()
            .iter()
            .map(|h| (h.field.to_string(), h.value.to_string()))
            .collect::<Vec<_>>();

        if is_stream {
            // 流式: 由 proxy 自行开线程转发并返回 pipe reader
            match crate::proxy::handle_proxy_streaming(
                Arc::clone(conn),
                &path,
                &headers,
                &body,
            ) {
                Ok((status, hdrs, reader)) => {
                    let mut resp =
                        tiny_http::Response::empty(status).with_data(reader, None);
                    for (k, v) in hdrs {
                        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                            resp = resp.with_header(h);
                        }
                    }
                    let _ = request.respond(resp);
                }
                Err(e) => {
                    let resp = tiny_http::Response::from_string(format!(
                        "{{\"error\":\"代理转发失败: {e}\"}}"
                    ))
                    .with_status_code(502)
                    .with_header(
                        tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                            .expect("h"),
                    );
                    let _ = request.respond(resp);
                }
            }
        } else {
            match crate::proxy::handle_proxy(conn, method.as_str(), &path, &headers, &body) {
                Ok((status, hdrs, bytes)) => {
                    let mut resp = tiny_http::Response::from_data(bytes).with_status_code(status);
                    for (k, v) in hdrs {
                        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                            resp = resp.with_header(h);
                        }
                    }
                    let _ = request.respond(resp);
                }
                Err(e) => {
                    let resp = tiny_http::Response::from_string(format!(
                        "{{\"error\":\"代理转发失败: {e}\"}}"
                    ))
                    .with_status_code(502)
                    .with_header(
                        tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                            .expect("h"),
                    );
                    let _ = request.respond(resp);
                }
            }
        }
        return;
    }

    let result = route(conn, &mut request, &method, &path);
    let (status, body) = match result {
        Ok((code, body)) => (code, body),
        Err(e) => (400, format!("{{\"ok\":false,\"error\":{}}}", serde_json::to_string(&e).unwrap_or_else(|_| "\"err\"".into()))),
    };
    let response = tiny_http::Response::from_string(body).with_status_code(status);
    // 允许任何来源的浏览器页面跨域调用(工具本就只绑回环)
    let response = response.with_header(
        tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..])
            .expect("valid header"),
    );
    let _ = request.respond(response);
}

fn route(
    conn: &Arc<Mutex<Connection>>,
    request: &mut tiny_http::Request,
    method: &tiny_http::Method,
    url: &str,
) -> Result<(u16, String), String> {
    let path = url.split('?').next().unwrap_or(url);
    match (method.as_str(), path) {
        ("GET", "/api/v1/ping") => Ok((
            200,
            json!({"ok": true, "service": "token-usage-tracker", "time": chrono::Utc::now().to_rfc3339()})
                .to_string(),
        )),
        ("POST", "/api/v1/usage") => {
            // 读取请求体(限制 1MB)
            let mut body = Vec::new();
            request
                .as_reader()
                .take(1024 * 1024)
                .read_to_end(&mut body)
                .map_err(|e| format!("读取请求体失败: {e}"))?;
            let report: UsageReport =
                serde_json::from_slice(&body).map_err(|e| format!("JSON 解析失败: {e}"))?;

            let guard = conn.lock().map_err(|e| format!("db lock: {e}"))?;
            // 当前汇率, 用于 cost_cny → USD 折算(统一以 USD 存账)
            let rate = read_usd_cny_rate(&guard);
            let rec = report_to_new_record(&report, rate)?;
            match crate::domain::records::insert(&guard, &rec, None) {
                Ok(Some(id)) => Ok((
                    200,
                    json!({"ok": true, "id": id, "source": "collector"}).to_string(),
                )),
                Ok(None) => Ok((
                    200,
                    json!({"ok": true, "duplicate": true, "source": "collector"}).to_string(),
                )),
                Err(e) => Err(format!("入库失败: {e}")),
            }
        }
        ("GET", "/") => Ok((
            200,
            "Token Usage Tracker collector\nPOST /api/v1/usage  JSON 用量上报\nGET  /api/v1/ping\n"
                .to_string(),
        )),
        ("OPTIONS", _) => Ok((204, String::new())),
        _ => Err(format!("不支持: {method} {path}")),
    }
}

fn report_to_new_record(r: &UsageReport, usd_cny_rate: f64) -> Result<crate::domain::records::NewRecord, String> {
    let model_name = r
        .model_name
        .clone()
        .or_else(|| r.model.clone())
        .unwrap_or_default();
    if model_name.trim().is_empty() {
        return Err("缺少必填字段 model_name(model)".into());
    }
    let provider_code = r
        .provider_code
        .clone()
        .or_else(|| r.provider.clone())
        .unwrap_or_default();

    // 币种折算: cost_cny(人民币) → USD; cost_usd 直接采用; 两者都无 → 由单价库估算
    let rate = if usd_cny_rate > 0.0 { usd_cny_rate } else { 7.1 };
    let cost_usd = match (r.cost_cny, r.cost_usd) {
        (Some(cny), _) => Some(cny / rate),
        (None, Some(usd)) => Some(usd),
        (None, None) => None,
    };

    let recorded_at = r.recorded_at.clone().unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    Ok(crate::domain::records::NewRecord {
        recorded_at,
        source: "collector".into(),
        provider_code: if provider_code.is_empty() { None } else { Some(provider_code) },
        model_name: Some(model_name),
        session_id: r.session_id.clone().filter(|s| !s.is_empty()),
        request_id: r.request_id.clone().filter(|s| !s.is_empty()),
        prompt_tokens: r.prompt_tokens,
        completion_tokens: r.completion_tokens,
        cached_tokens: r.cached_tokens,
        cost_usd,
        cost_source: if cost_usd.is_some() { Some("official".into()) } else { None },
        project: None,
        tags: None,
        note: r.note.clone().filter(|s| !s.is_empty()),
    })
}

/// 从 kv_settings 读取 USD→CNY 汇率, 失败返回默认 7.1。
fn read_usd_cny_rate(conn: &Connection) -> f64 {
    conn.query_row(
        "SELECT value FROM kv_settings WHERE key = 'usd_cny_rate'",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| s.parse::<f64>().ok())
    .filter(|v| *v > 0.0)
    .unwrap_or(7.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::domain::records;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::AtomicBool;

    fn http_post(port: u16, body: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST /api/v1/usage HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        buf
    }

    #[test]
    fn report_validation_and_currency() {
        let r = UsageReport::default();
        assert!(report_to_new_record(&r, 7.1).is_err()); // 缺 model
        let r = UsageReport { model: Some("deepseek-chat".into()), prompt_tokens: Some(10), ..Default::default() };
        let rec = report_to_new_record(&r, 7.1).unwrap();
        assert_eq!(rec.model_name.as_deref(), Some("deepseek-chat"));
        assert_eq!(rec.source, "collector");
        assert!(rec.cost_usd.is_none());

        // cost_cny 折算: 71 CNY / 7.1 = 10 USD
        let r = UsageReport { model: Some("m".into()), cost_cny: Some(71.0), ..Default::default() };
        let rec = report_to_new_record(&r, 7.1).unwrap();
        assert_eq!(rec.cost_usd, Some(10.0));
        assert_eq!(rec.cost_source.as_deref(), Some("official"));
    }

    #[test]
    fn http_collect_roundtrip() {
        // 真实库 + 真实 HTTP: 起服务在随机端口, 发 POST, 断言入库与去重
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        let conn = db::open(&db_path).unwrap();
        db::migrate(&conn).unwrap();
        let conn = Arc::new(Mutex::new(conn));
        let stop = Arc::new(AtomicBool::new(false));

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let stop2 = Arc::clone(&stop);
        let conn2 = Arc::clone(&conn);
        std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                match server.recv_timeout(std::time::Duration::from_millis(300)) {
                    Ok(Some(req)) => handle_request(&conn2, req),
                    _ => continue,
                }
            }
        });

        let body1 = r#"{"model":"deepseek-chat","provider":"deepseek","prompt_tokens":100,"completion_tokens":50,"request_id":"r1"}"#;
        let resp1 = http_post(port, body1);
        assert!(resp1.contains("\"ok\":true"), "resp: {resp1}");

        {
            let conn_guard = conn.lock().unwrap();
            let n = records::count(&conn_guard, &records::RecordFilter::default()).unwrap();
            assert_eq!(n, 1);
        }

        // 同 request_id 去重
        let resp2 = http_post(port, body1);
        assert!(resp2.contains("duplicate"), "resp: {resp2}");
        {
            let conn_guard = conn.lock().unwrap();
            let n = records::count(&conn_guard, &records::RecordFilter::default()).unwrap();
            assert_eq!(n, 1);
        }

        // 缺 model → 400
        let resp3 = http_post(port, r#"{"prompt_tokens":1}"#);
        assert!(resp3.contains("model_name"), "resp: {resp3}");

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
