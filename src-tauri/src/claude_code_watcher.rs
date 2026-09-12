//! Claude Code 本机用量自动检测器。
//!
//! Claude Code 把每次会话的完整事件流以 JSONL 追加写入:
//!   `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`
//! 其中 `type=assistant` 的行携带本次 LLM 调用的真实用量:
//! ```json
//! {
//!   "type": "assistant",
//!   "uuid": "<row-uuid>",
//!   "timestamp": "2026-09-05T08:44:29.182Z",
//!   "cwd": "C:\\Users\\31907\\Desktop\\agent_test",
//!   "sessionId": "<session-uuid>",
//!   "message": {
//!     "id": "<message-uuid>",
//!     "model": "claude-sonnet-4-5" | "MiniMax-M3" | ...,
//!     "usage": {
//     "input_tokens": 32792,
//!       "cache_creation_input_tokens": 0,
//!       "cache_read_input_tokens": 256,
//!       "output_tokens": 121,
//!       ...
//!     }
//!   }
//! }
//! ```
//!
//! 一次 LLM 调用可能被切成多行(text / thinking / tool_use), 它们共享同一个
//! `message.id` 与同一份 `usage`。检测器以"追加读 + 已见 message.id 去重"
//! 实现增量: 记住每个文件的 `last_byte_offset`, 以及已入库的 message.id。
//!
//! 语义对齐 DSH watcher:
//!   prompt_tokens = input_tokens + cache_creation_input_tokens + cache_read_input_tokens
//!   cached_tokens  = cache_read_input_tokens(单列, 用于分开计价)
//!   completion_tokens = output_tokens
//!   source='claude_code', request_id='claude-{session}-{messageId}'
//!   provider_code 与 model_name 原样存储 jsonl 中的值, 价格库未命中则估算为 0。

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

/// 启动结果: 探测到的根路径与是否成功启动线程(供前端展示)。
#[derive(Clone, serde::Serialize)]
pub struct WatcherInfo {
    pub claude_home: String,
    pub started: bool,
    pub error: Option<String>,
}

/// 解析后的一条 assistant 调用(可能是一次 LLM response 的多行之一)
#[derive(Debug, Deserialize)]
struct AssistantRow {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<AssistantMessage>,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct Usage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
}

/// 一条 assistant 行的解析结果(可入库)
#[derive(Debug)]
struct ParsedCall {
    message_id: String,
    row_uuid: Option<String>,
    timestamp: String,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
}

/// 枚举候选行(宽松解析): 顶层 type=assistant 才需要继续看 usage。
fn parse_assistant_line(line: &str) -> Option<ParsedCall> {
    // 快速过滤: 不是 assistant 行直接返回
    if !line.contains("\"type\":\"assistant\"") {
        return None;
    }
    let row: AssistantRow = serde_json::from_str(line).ok()?;
    let msg = row.message?;
    let usage = msg.usage.unwrap_or_default();
    let message_id = match msg.id {
        Some(s) if !s.is_empty() => s,
        _ => return None, // 没有 message.id 无法去重, 视为不可入库
    };
    let timestamp = row.timestamp.unwrap_or_default();
    if timestamp.is_empty() {
        return None;
    }
    let input = usage.input_tokens.unwrap_or(0);
    let output = usage.output_tokens.unwrap_or(0);
    let cc = usage.cache_creation_input_tokens.unwrap_or(0);
    let cr = usage.cache_read_input_tokens.unwrap_or(0);
    // 全部为 0: 纯结构事件, 入库无意义
    if input + output + cc + cr == 0 {
        return None;
    }
    Some(ParsedCall {
        message_id,
        row_uuid: row.uuid,
        timestamp,
        session_id: row.session_id,
        cwd: row.cwd,
        model: msg.model,
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cc,
        cache_read_input_tokens: cr,
    })
}

fn insert_call(conn: &Connection, p: &ParsedCall) {
    let prompt = p.input_tokens + p.cache_creation_input_tokens + p.cache_read_input_tokens;
    let cached = p.cache_read_input_tokens;
    let session_id = p.session_id.clone().filter(|s| !s.is_empty());
    let request_id = format!(
        "claude-{}-{}",
        session_id.clone().unwrap_or_else(|| "unknown".into()),
        p.message_id
    );
    let rec = NewRecord {
        recorded_at: p.timestamp.clone(),
        source: "claude_code".into(),
        // 模型名原样存(不做别名映射)。
        // Claude Code 的 jsonl 不含厂商字段, 故 provider 留空 —— 早期版本误把模型名
        // 写进 provider_code, 导致明细渲染成 "MiniMax-M3 MiniMax-M3" 并把模型当厂商计数
        // (历史脏数据由 db 迁移 v3 清理)。
        provider_code: None,
        model_name: p.model.clone().filter(|s| !s.is_empty()),
        session_id,
        request_id: Some(request_id),
        prompt_tokens: Some(prompt),
        completion_tokens: Some(p.output_tokens),
        cached_tokens: Some(cached),
        cost_usd: None,
        cost_source: None,
        project: p.cwd.clone().filter(|s| !s.is_empty()),
        tags: None,
        note: p.row_uuid.as_ref().map(|u| format!("Claude Code row {}", u)),
    };
    let _ = crate::domain::records::insert(conn, &rec, None);
}

/// 水位线状态: 每个文件记住读取偏移 + 已入库的 message.id(防同 message 跨多行重复)
#[derive(Default)]
pub struct FileState {
    offset: u64,
    seen_message_ids: HashSet<String>,
}

/// 一次性扫描: 返回本次新增条数。
pub fn scan_once(
    conn: &Connection,
    claude_home: &Path,
    states: &mut HashMap<PathBuf, FileState>,
) -> Result<usize, String> {
    let projects_dir = claude_home.join("projects");
    let entries = std::fs::read_dir(&projects_dir)
        .map_err(|e| format!("读取 {} 失败: {e}", projects_dir.display()))?;
    let mut added = 0usize;

    for proj_entry in entries.flatten() {
        let proj_path = proj_entry.path();
        if !proj_path.is_dir() {
            continue;
        }
        let proj_files = match std::fs::read_dir(&proj_path) {
            Ok(it) => it,
            Err(_) => continue, // 单个项目目录读不到不影响其他
        };
        for fentry in proj_files.flatten() {
            let path = fentry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            added += scan_file(conn, &path, states);
        }
    }
    Ok(added)
}

fn scan_file(
    conn: &Connection,
    path: &Path,
    states: &mut HashMap<PathBuf, FileState>,
) -> usize {
    // 1) 拿当前文件大小与状态
    let cur_size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return 0,
    };
    let state = states.entry(path.to_path_buf()).or_default();

    // 2) 文件被截断/重置: 偏移大于当前大小 → 重读(用 request_id 兜底去重)
    if state.offset > cur_size {
        state.offset = 0;
        state.seen_message_ids.clear();
    }
    if state.offset == cur_size {
        return 0; // 没有新内容
    }

    // 3) 读 [offset, cur_size) 区间; 用 File::seek 跳过已读字节
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    if file.seek(SeekFrom::Start(state.offset)).is_err() {
        return 0;
    }
    let mut buf = Vec::with_capacity((cur_size - state.offset).min(1024 * 1024) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return 0;
    }

    // 4) 解析完整行(以 \n 结尾)。行 = 从上一个 \n 之后到下一个 \n 之前(不含 \n)。
    //    末尾无 \n 的不完整行不解析, 等下次追加后再来。
    let mut added = 0usize;
    let mut scan_pos: usize = 0;
    while scan_pos < buf.len() {
        // 找下一个 \n
        let newline_pos = match buf[scan_pos..].iter().position(|b| *b == b'\n') {
            Some(p) => scan_pos + p,
            None => break, // 没找到 \n, 剩下的是不完整半行, 不解析不推进
        };
        // 提取 [scan_pos, newline_pos) 即为一行内容
        let line_bytes = &buf[scan_pos..newline_pos];
        // 去掉行尾的 \r(Windows 兼容)
        let line_end = if line_bytes.last() == Some(&b'\r') {
            line_bytes.len() - 1
        } else {
            line_bytes.len()
        };
        let line_slice = &buf[scan_pos..scan_pos + line_end];
        if let Ok(line_str) = std::str::from_utf8(line_slice) {
            if let Some(call) = parse_assistant_line(line_str) {
                if !state.seen_message_ids.contains(&call.message_id) {
                    state.seen_message_ids.insert(call.message_id.clone());
                    insert_call(conn, &call);
                    added += 1;
                }
            }
        }
        // 推进到 \n 之后
        scan_pos = newline_pos + 1;
    }

    // 5) 推进 offset: 推进到已扫描到的位置
    state.offset = state.offset + scan_pos as u64;

    // 防 seen set 单调膨胀: 单文件上限 50k(覆盖 ~几个月同一会话)
    if state.seen_message_ids.len() > 50_000 {
        state.seen_message_ids.clear();
    }

    added
}

/// 启动后台检测线程(每 poll_ms 扫描一次)。
pub fn start_watcher(
    conn: Arc<Mutex<Connection>>,
    claude_home: PathBuf,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("claude-code-watcher".into())
        .spawn(move || {
            let mut states: HashMap<PathBuf, FileState> = HashMap::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(guard) = conn.lock() {
                    if let Err(e) = scan_once(&guard, &claude_home, &mut states) {
                        eprintln!("[claude-code-watcher] {e}");
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
        .map_err(|e| format!("Claude Code 检测线程启动失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// 构造一条完整的 assistant 行
    fn assistant_line(
        message_id: &str,
        uuid: &str,
        ts: &str,
        session: &str,
        cwd: &str,
        model: &str,
        inp: i64,
        out: i64,
        cc: i64,
        cr: i64,
    ) -> String {
        format!(
            r#"{{"parentUuid":"x","type":"assistant","uuid":"{uuid}","timestamp":"{ts}","sessionId":"{session}","cwd":"{cwd}","message":{{"id":"{message_id}","type":"message","role":"assistant","model":"{model}","usage":{{"input_tokens":{inp},"output_tokens":{out},"cache_creation_input_tokens":{cc},"cache_read_input_tokens":{cr}}}}}}}"#,
            message_id = message_id,
            uuid = uuid,
            ts = ts,
            session = session,
            cwd = cwd.replace('\\', "\\\\"),
            model = model,
            inp = inp,
            out = out,
            cc = cc,
            cr = cr,
        )
    }

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}}}}"#,
            text
        )
    }

    fn open_test_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.db")).unwrap();
        db::migrate(&conn).unwrap();
        (dir, conn)
    }

    fn count_claude_records(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM usage_record WHERE source='claude_code'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn parses_only_assistant_with_usage() {
        let p = parse_assistant_line(&user_line("hi"));
        assert!(p.is_none(), "user 行不应被解析");

        // assistant 但 usage 全 0: 不入库
        let z = assistant_line("m1", "u1", "2026-09-05T08:44:29.182Z", "s1", "C:/x", "claude-x", 0, 0, 0, 0);
        assert!(parse_assistant_line(&z).is_none());

        // assistant 有 usage
        let good = assistant_line(
            "m1",
            "u1",
            "2026-09-05T08:44:29.182Z",
            "s1",
            "C:/x",
            "claude-x",
            100,
            50,
            20,
            10,
        );
        let p = parse_assistant_line(&good).expect("good");
        assert_eq!(p.message_id, "m1");
        assert_eq!(p.input_tokens, 100);
        assert_eq!(p.cache_read_input_tokens, 10);
    }

    #[test]
    fn scan_inserts_increments_only() {
        let (dir, conn) = open_test_db();
        let proj = dir.path().join("projects").join("projA");
        std::fs::create_dir_all(&proj).unwrap();
        let f = proj.join("session-s1.jsonl");

        // 第一次: 一条 assistant 一次调用(text 分块)
        let l1 = assistant_line(
            "m1",
            "u1",
            "2026-09-05T08:44:29.182Z",
            "s1",
            "C:/proj",
            "claude-sonnet",
            1000,
            500,
            0,
            200,
        );
        std::fs::write(&f, format!("{l1}\n")).unwrap();
        let claude_home = dir.path();
        let mut states = HashMap::new();
        let n = scan_once(&conn, claude_home, &mut states).unwrap();
        assert_eq!(n, 1);
        assert_eq!(count_claude_records(&conn), 1);

        // 追加同一 message.id 的 tool_use 分块: 已被 seen, 不再入库
        let l2 = assistant_line(
            "m1",
            "u2",
            "2026-09-05T08:44:29.412Z",
            "s1",
            "C:/proj",
            "claude-sonnet",
            1000,
            500,
            0,
            200,
        );
        let mut content = std::fs::read_to_string(&f).unwrap();
        content.push_str(&format!("{l2}\n"));
        std::fs::write(&f, &content).unwrap();
        let n = scan_once(&conn, claude_home, &mut states).unwrap();
        assert_eq!(n, 0, "同 message.id 应去重");
        assert_eq!(count_claude_records(&conn), 1);

        // 追加新的 message.id: 入库
        let l3 = assistant_line(
            "m2",
            "u3",
            "2026-09-05T08:44:30.172Z",
            "s1",
            "C:/proj",
            "claude-sonnet",
            50,
            20,
            0,
            0,
        );
        content.push_str(&format!("{l3}\n"));
        std::fs::write(&f, &content).unwrap();
        let n = scan_once(&conn, claude_home, &mut states).unwrap();
        assert_eq!(n, 1);
        assert_eq!(count_claude_records(&conn), 2);

        // 验证 token 口径: prompt = input + cc + cr
        let (prompt, comp, cached): (i64, i64, i64) = conn
            .query_row(
                "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(cached_tokens),0)
                 FROM usage_record WHERE source='claude_code'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        // m1: prompt=1000+0+200=1200, out=500, cached=200
        // m2: prompt=50, out=20, cached=0
        assert_eq!(prompt, 1250);
        assert_eq!(comp, 520);
        assert_eq!(cached, 200);
    }

    #[test]
    fn scan_handles_truncated_file() {
        let (dir, conn) = open_test_db();
        let proj = dir.path().join("projects").join("projA");
        std::fs::create_dir_all(&proj).unwrap();
        let f = proj.join("session-s1.jsonl");

        let l1 = assistant_line(
            "m1",
            "u1",
            "2026-09-05T08:44:29.182Z",
            "s1",
            "C:/proj",
            "claude-x",
            10,
            5,
            0,
            0,
        );
        std::fs::write(&f, format!("{l1}\n")).unwrap();
        let mut states = HashMap::new();
        scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(count_claude_records(&conn), 1);

        // 模拟 truncate 后写入更小的新内容: 偏移(316) > 新大小(<316) → 触发重读
        let l2_short = assistant_line(
            "m2",
            "u2",
            "2026-09-05T08:44:30.000Z",
            "s1",
            "C:/proj",
            "claude-x",
            5,
            2,
            0,
            0,
        );
        assert!(l2_short.len() < l1.len(), "l2 必须比 l1 短才能触发 offset > cur_size 分支");
        std::fs::write(&f, format!("{l2_short}\n")).unwrap();
        let n = scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(n, 1, "truncate 后应重新扫描");
        // 入库 2 条(request_id 唯一性保证不会撞唯一索引)
        assert_eq!(count_claude_records(&conn), 2);
    }

    #[test]
    fn scan_skips_non_jsonl_and_non_projects_dirs() {
        let (dir, conn) = open_test_db();
        // 在 projects 下塞一个 txt: 应被跳过
        let proj = dir.path().join("projects");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("readme.txt"), "noise").unwrap();
        // 在 projects 下塞一个非 jsonl 文件: 应被跳过
        let proj_a = proj.join("projA");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::write(proj_a.join("meta.json"), "{}").unwrap();

        let mut states = HashMap::new();
        let n = scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn partial_line_not_consumed() {
        // 验证: 半行 JSON 不会被吞, 下次扫描时能拼上
        let (dir, conn) = open_test_db();
        let proj = dir.path().join("projects").join("projA");
        std::fs::create_dir_all(&proj).unwrap();
        let f = proj.join("session-s1.jsonl");

        // 写一个不完整的 assistant 行(模拟写入中途): 没有 \n
        let partial = r#"{"type":"assistant","uuid":"u1","timestamp":"2026-09-05T08:44:29.182Z","sessionId":"s1","message":{"id":"m1","usage":{"input_tok"#;
        std::fs::write(&f, partial).unwrap();

        let mut states = HashMap::new();
        let n = scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(n, 0, "半行不应入库");

        // 补齐尾部 + \n: partial 结尾是 "input_tok" 缺 "ens":10,"output_tokens":5",
// 然后闭合 usage { }、闭合 message { }、闭合最外层 { }  共 3 个 }
        let tail = r#"ens":10,"output_tokens":5}}}"#;
        let mut full = partial.as_bytes().to_vec();
        full.extend_from_slice(tail.as_bytes());
        full.push(b'\n');
        std::fs::write(&f, &full).unwrap();
        let n = scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(n, 1, "补齐后应入库");
    }
}