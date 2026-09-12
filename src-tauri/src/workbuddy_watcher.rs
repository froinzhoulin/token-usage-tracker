//! WorkBuddy 本机用量自动检测器。
//!
//! WorkBuddy 把会话事件流以 JSONL 追加写入:
//!   `~/.workbuddy/projects/<编码后的工作目录>/<会话 uuid>.jsonl`
//!
//! 携带用量的是 `providerData.rawUsage` 非空的行 —— 注意**行类型不止一种**,
//! 实测 `type=message`(role=assistant) 与 `type=function_call` 都可能带用量:
//! ```json
//! {"id":"gen-...","timestamp":1789196249295,"type":"message","role":"assistant",
//!  "sessionId":"c2d2c5e0-...","cwd":"c:\\Users\\...\\WorkBuddy\\2026-09-12-14-56-56",
//!  "providerData":{"messageId":"8956ca74...","model":"hy4-preview-f",
//!    "traceId":"b476640c...","rawUsage":{
//!      "prompt_tokens":30547,"completion_tokens":384,"total_tokens":30931,
//!      "prompt_cache_hit_tokens":12736,"prompt_cache_miss_tokens":17811,
//!      "prompt_tokens_details":{"cached_tokens":12736},
//!      "completion_tokens_details":{"reasoning_tokens":219},
//!      "credit":0}}}
//! ```
//!
//! ## 口径(已在本机真实数据上核对)
//!
//! - `prompt_cache_hit_tokens + prompt_cache_miss_tokens == prompt_tokens`
//!   → **prompt_tokens 已包含缓存命中, 不可再相加**(与 DSH 口径相反)
//! - `total_tokens == prompt_tokens + completion_tokens`
//! - `completion_tokens_details.reasoning_tokens` 是 output 的子集
//! - 用量为**单次调用值**, 非累计(同一会话内 prompt 随上下文增长)
//!
//! ## 去重
//!
//! `providerData.messageId` 每次调用唯一(实测 4 条各不相同), 直接作为 `request_id`。
//! 注意 `traceId` 会被同一轮的多次调用**共用**, 不能当去重键。
//!
//! 时间戳是**毫秒 epoch**, 入库前转 ISO8601。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Deserialize;

use crate::domain::records::NewRecord;

/// 默认轮询间隔(ms)
pub const DEFAULT_POLL_MS: u64 = 3000;

/// 启动结果(供前端展示)。
#[derive(Clone, serde::Serialize)]
pub struct WatcherInfo {
    pub workbuddy_home: String,
    pub started: bool,
    pub error: Option<String>,
}

// ---------------- 行解析 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WbLine {
    /// 毫秒 epoch
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    provider_data: Option<ProviderData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderData {
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    raw_usage: Option<RawUsage>,
}

/// 注意: rawUsage 内部字段是 snake_case, 不能套 camelCase 重命名。
#[derive(Debug, Clone, Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<i64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: Option<i64>,
}

impl RawUsage {
    fn prompt(&self) -> i64 {
        self.prompt_tokens.unwrap_or(0)
    }
    fn completion(&self) -> i64 {
        self.completion_tokens.unwrap_or(0)
    }
    /// 缓存命中: 优先 prompt_cache_hit_tokens, 回退 prompt_tokens_details.cached_tokens
    fn cached(&self) -> i64 {
        self.prompt_cache_hit_tokens
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
            })
            .unwrap_or(0)
    }
    fn reasoning(&self) -> i64 {
        self.completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0)
    }
}

/// 一条可入库的调用
#[derive(Debug)]
struct ParsedCall {
    message_id: String,
    timestamp_ms: Option<i64>,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    usage: RawUsage,
}

/// 解析一行; 非用量行返回 None。
/// 先用廉价子串过滤, 避免为体积巨大的工具输出行做完整 JSON 解析。
fn parse_line(line: &str) -> Option<ParsedCall> {
    if !line.contains("\"rawUsage\"") {
        return None;
    }
    let row: WbLine = serde_json::from_str(line).ok()?;
    let pd = row.provider_data?;
    let usage = pd.raw_usage?;
    if usage.prompt() + usage.completion() <= 0 {
        return None; // 空调用
    }
    let message_id = match pd.message_id {
        Some(s) if !s.is_empty() => s,
        _ => return None, // 无 messageId 无法去重
    };
    Some(ParsedCall {
        message_id,
        timestamp_ms: row.timestamp,
        session_id: row.session_id,
        cwd: row.cwd,
        model: pd.model,
        usage,
    })
}

// ---------------- 水位线状态 ----------------

#[derive(Default)]
pub struct FileState {
    offset: u64,
    seen_message_ids: HashSet<String>,
}

fn insert_call(conn: &Connection, p: &ParsedCall) -> bool {
    let recorded_at = p
        .timestamp_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let reasoning = p.usage.reasoning();
    let note = if reasoning > 0 {
        format!("WorkBuddy 调用(推理 {reasoning} tokens)")
    } else {
        "WorkBuddy 调用".to_string()
    };
    let rec = NewRecord {
        recorded_at,
        source: "workbuddy".into(),
        // 行内无厂商字段, 与 Claude Code 一致: 厂商留空, 模型名原样存
        provider_code: None,
        model_name: p.model.clone().filter(|s| !s.is_empty()),
        session_id: p.session_id.clone().filter(|s| !s.is_empty()),
        // messageId 每次调用唯一 → 天然去重键
        request_id: Some(format!("workbuddy-{}", p.message_id)),
        // prompt_tokens 已含缓存命中, 不可相加
        prompt_tokens: Some(p.usage.prompt()),
        // completion_tokens 已含推理 token
        completion_tokens: Some(p.usage.completion()),
        cached_tokens: Some(p.usage.cached()),
        cost_usd: None,
        cost_source: None,
        project: p.cwd.clone().filter(|s| !s.is_empty()),
        tags: None,
        note: Some(note),
    };
    crate::domain::records::insert(conn, &rec, None).is_ok()
}

/// 递归收集 projects 下的 *.jsonl
fn collect_session_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_session_files(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

/// 单轮扫描: 返回本次新增条数。
pub fn scan_once(
    conn: &Connection,
    workbuddy_home: &Path,
    states: &mut HashMap<PathBuf, FileState>,
) -> Result<usize, String> {
    let projects = workbuddy_home.join("projects");
    if !projects.is_dir() {
        return Err(format!("未找到 {} 目录", projects.display()));
    }
    let mut files = Vec::new();
    collect_session_files(&projects, &mut files);

    // 整轮一个事务: 首次扫描可能补录大量历史会话
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut added = 0usize;
    for f in files {
        added += scan_file(&tx, &f, states);
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(added)
}

fn scan_file(conn: &Connection, path: &Path, states: &mut HashMap<PathBuf, FileState>) -> usize {
    let cur_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return 0,
    };
    let state = states.entry(path.to_path_buf()).or_default();
    if state.offset > cur_size {
        *state = FileState::default(); // 文件被截断 → 重置
    }
    if state.offset == cur_size {
        return 0;
    }

    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    if file.seek(SeekFrom::Start(state.offset)).is_err() {
        return 0;
    }
    let mut buf = Vec::with_capacity((cur_size - state.offset).min(4 * 1024 * 1024) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return 0;
    }

    let mut added = 0usize;
    let mut scan_pos: usize = 0;
    while scan_pos < buf.len() {
        // 只处理完整行; 末尾半行留给下次
        let nl = match buf[scan_pos..].iter().position(|b| *b == b'\n') {
            Some(p) => scan_pos + p,
            None => break,
        };
        let raw = &buf[scan_pos..nl];
        let end = if raw.last() == Some(&b'\r') { raw.len() - 1 } else { raw.len() };
        if let Ok(line) = std::str::from_utf8(&buf[scan_pos..scan_pos + end]) {
            if let Some(call) = parse_line(line) {
                if !state.seen_message_ids.contains(&call.message_id) {
                    state.seen_message_ids.insert(call.message_id.clone());
                    if insert_call(conn, &call) {
                        added += 1;
                    }
                }
            }
        }
        scan_pos = nl + 1;
    }
    state.offset += scan_pos as u64;
    if state.seen_message_ids.len() > 50_000 {
        state.seen_message_ids.clear();
    }
    added
}

/// 启动后台检测线程(每 poll_ms 扫描一次)。
pub fn start_watcher(
    conn: Arc<Mutex<Connection>>,
    workbuddy_home: PathBuf,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("workbuddy-watcher".into())
        .spawn(move || {
            let mut states: HashMap<PathBuf, FileState> = HashMap::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(guard) = conn.lock() {
                    if let Err(e) = scan_once(&guard, &workbuddy_home, &mut states) {
                        eprintln!("[workbuddy-watcher] {e}");
                    }
                }
                let mut waited = 0u64;
                while waited < poll_ms {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    waited += 200;
                }
            }
        })
        .map_err(|e| format!("WorkBuddy 检测线程启动失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// 构造一条带用量的行(格式与真实 jsonl 一致)
    fn usage_line(
        ts_ms: i64,
        msg_id: &str,
        model: &str,
        session: &str,
        cwd: &str,
        prompt: i64,
        completion: i64,
        hit: i64,
        reasoning: i64,
    ) -> String {
        format!(
            concat!(
                r#"{{"id":"gen-{ts}","timestamp":{ts},"type":"message","role":"assistant","status":"completed","#,
                r#""sessionId":"{s}","cwd":"{c}","providerData":{{"messageId":"{m}","model":"{mo}","#,
                r#""traceId":"trace-1","rawUsage":{{"prompt_tokens":{p},"completion_tokens":{co},"total_tokens":{tt},"#,
                r#""prompt_cache_hit_tokens":{h},"prompt_cache_miss_tokens":{miss},"#,
                r#""prompt_tokens_details":{{"cached_tokens":{h}}},"#,
                r#""completion_tokens_details":{{"reasoning_tokens":{r}}},"credit":0}}}}}}"#
            ),
            ts = ts_ms,
            s = session,
            c = cwd.replace('\\', "\\\\"),
            m = msg_id,
            mo = model,
            p = prompt,
            co = completion,
            tt = prompt + completion,
            h = hit,
            miss = prompt - hit,
            r = reasoning,
        )
    }

    fn open_test_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.db")).unwrap();
        db::migrate(&conn).unwrap();
        (dir, conn)
    }

    fn write_session(root: &Path, dir_name: &str, file: &str, content: &str) -> PathBuf {
        let d = root.join("projects").join(dir_name);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(file);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn count_wb(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM usage_record WHERE source='workbuddy'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn parses_usage_line() {
        let l = usage_line(
            1_789_196_249_295,
            "msg-1",
            "hy4-preview-f",
            "sess-1",
            "c:\\Users\\x\\WorkBuddy\\p",
            30547,
            384,
            12736,
            219,
        );
        let c = parse_line(&l).expect("应解析出用量");
        assert_eq!(c.message_id, "msg-1");
        assert_eq!(c.model.as_deref(), Some("hy4-preview-f"));
        assert_eq!(c.usage.prompt(), 30547);
        assert_eq!(c.usage.completion(), 384);
        assert_eq!(c.usage.cached(), 12736);
        assert_eq!(c.usage.reasoning(), 219);
        assert_eq!(c.session_id.as_deref(), Some("sess-1"));

        // 无 rawUsage 的行(如 user 消息)不应被解析
        assert!(parse_line(r#"{"type":"message","role":"user","content":[]}"#).is_none());
    }

    #[test]
    fn scan_inserts_with_correct_fields() {
        let (dir, conn) = open_test_db();
        let content = format!(
            "{}\n",
            usage_line(
                1_789_196_249_295,
                "msg-1",
                "hy4-preview-f",
                "sess-1",
                "c:\\Users\\x\\WorkBuddy\\p",
                30547,
                384,
                12736,
                219,
            )
        );
        write_session(dir.path(), "c-Users-x-WorkBuddy-p", "sess-1.jsonl", &content);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);

        let (model, session, project, prompt, comp, cached, rid): (
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT model_name, session_id, project, prompt_tokens, completion_tokens,
                        cached_tokens, request_id
                 FROM usage_record WHERE source='workbuddy'",
                [],
                |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(model.as_deref(), Some("hy4-preview-f"));
        assert_eq!(session.as_deref(), Some("sess-1"));
        assert_eq!(project.as_deref(), Some("c:\\Users\\x\\WorkBuddy\\p"));
        // 关键口径: prompt 已含缓存, 不是 prompt + hit
        assert_eq!(prompt, 30547, "prompt_tokens 已包含缓存命中, 不可相加");
        assert_eq!(cached, 12736);
        assert_eq!(comp, 384);
        assert_eq!(rid.as_deref(), Some("workbuddy-msg-1"));

        // 时间戳为毫秒 epoch, 应转成正确的 UTC 时刻
        let at: String = conn
            .query_row(
                "SELECT recorded_at FROM usage_record WHERE source='workbuddy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expect = chrono::DateTime::from_timestamp_millis(1_789_196_249_295)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        assert_eq!(at, expect);
    }

    #[test]
    fn both_message_and_function_call_lines_are_recorded() {
        // 实测用量同时出现在 message 与 function_call 两类行上
        let (dir, conn) = open_test_db();
        let a = usage_line(1_789_196_249_295, "m1", "hy4-preview-f", "s", "c:\\w", 100, 10, 0, 0);
        let b = usage_line(1_789_196_344_414, "m2", "hy4-preview-f", "s", "c:\\w", 200, 20, 50, 5)
            .replace("\"type\":\"message\"", "\"type\":\"function_call\"");
        write_session(dir.path(), "d", "s.jsonl", &format!("{a}\n{b}\n"));

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 2);
        assert_eq!(count_wb(&conn), 2);
    }

    #[test]
    fn incremental_read_and_no_duplicates() {
        let (dir, conn) = open_test_db();
        let first = usage_line(1_789_196_249_295, "m1", "m", "s", "c:\\w", 100, 10, 0, 0);
        let f = write_session(dir.path(), "d", "s.jsonl", &format!("{first}\n"));

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 0, "无新内容");

        // 追加同一 messageId 的重复行: 不应重复入库
        let dup = usage_line(1_789_196_249_295, "m1", "m", "s", "c:\\w", 100, 10, 0, 0);
        let mut c = std::fs::read_to_string(&f).unwrap();
        c.push_str(&format!("{dup}\n"));
        std::fs::write(&f, c).unwrap();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 0, "同 messageId 去重");

        // 追加新调用
        let second = usage_line(1_789_196_344_414, "m2", "m", "s", "c:\\w", 300, 30, 0, 0);
        let mut c = std::fs::read_to_string(&f).unwrap();
        c.push_str(&format!("{second}\n"));
        std::fs::write(&f, c).unwrap();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        assert_eq!(count_wb(&conn), 2);
    }

    #[test]
    fn partial_line_not_consumed() {
        let (dir, conn) = open_test_db();
        let full = usage_line(1_789_196_249_295, "m1", "m", "s", "c:\\w", 100, 10, 0, 0);
        let cut = full.len() / 2;
        let f = write_session(dir.path(), "d", "s.jsonl", &full[..cut]);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 0, "半行不入库");

        std::fs::write(&f, format!("{full}\n")).unwrap();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1, "补齐后入库");
    }

    /// 端到端(手动触发): 扫描真实 ~/.workbuddy 到临时库, 不触碰应用库。
    /// 运行: cargo test --lib scan_real_workbuddy_home -- --ignored --nocapture
    #[test]
    #[ignore]
    fn scan_real_workbuddy_home() {
        let home = match std::env::var("WORKBUDDY_HOME") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
                Ok(b) => std::path::PathBuf::from(b).join(".workbuddy"),
                Err(_) => return,
            },
        };
        if !home.join("projects").is_dir() {
            eprintln!("[wb-e2e] skip: {} 不存在", home.join("projects").display());
            return;
        }

        let (_dir, conn) = open_test_db();
        let mut states = HashMap::new();
        let n = scan_once(&conn, &home, &mut states).unwrap();
        let (calls, p, c, ca): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(cached_tokens),0)
                 FROM usage_record WHERE source='workbuddy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        eprintln!("[wb-e2e] 首次扫描新增 = {n}");
        eprintln!("[wb-e2e] 调用={calls} 输入={p} 输出={c} 缓存={ca}");
        assert_eq!(scan_once(&conn, &home, &mut states).unwrap(), 0, "第二轮不应新增");
    }
}
