//! Codex(OpenAI Codex CLI / IDE 扩展)本机用量自动检测器。
//!
//! Codex 把每个会话的完整事件流以 JSONL 追加写入:
//!   `~/.codex/sessions/<年>/<月>/<日>/rollout-<时间戳>-<会话uuid>.jsonl`
//!
//! 其中一次 LLM 调用对应一行 `type=event_msg` 且 `payload.type=token_count`:
//! ```json
//! {"timestamp":"2026-09-01T11:54:54.761Z","type":"event_msg",
//!  "payload":{"type":"token_count","info":{
//!    "total_token_usage":{"input_tokens":12027,"cached_input_tokens":11008,
//!                         "cache_write_input_tokens":0,"output_tokens":226,
//!                         "reasoning_output_tokens":155,"total_tokens":12253},
//!    "last_token_usage":{ ...同结构... },
//!    "model_context_window":258400}}}
//! ```
//!
//! ## 口径(已在本机 52 个会话、3676 条真实事件上验证)
//!
//! - `total_tokens == input_tokens + output_tokens`(557/557 成立)
//! - `cached_input_tokens <= input_tokens` —— **缓存是 input 的子集, 不能相加!**
//!   这一点与 DSH 相反(DSH 的 prompt = 未缓存 + 缓存, 两桶独立)
//! - `reasoning_output_tokens <= output_tokens` —— 推理是 output 的子集
//!
//! ## 算法
//!
//! 1. 取 `last_token_usage`(单次调用用量), **不用** `total_token_usage` 当会话总量 ——
//!    Codex 支持会话 resume, 此时 total 会继承上一个会话(实测有文件首条 total 即 2887 万)。
//! 2. 若累计值 `total_tokens` 未增长(delta == 0), 该事件是**重复上报**, 跳过。
//!    实测 557 条事件里有 4 条此类, 不跳过会虚增 38,287 tokens。
//! 3. 每个文件的首条 token_count 事件直接采用(新会话或 resume 会话都正确)。
//!
//! 模型名不在 token 事件里: 来自 `turn_context.payload.model`;
//! 厂商/会话/工作目录来自 `session_meta.payload.{model_provider,session_id,cwd}`。

use std::collections::HashMap;
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
    pub codex_home: String,
    pub started: bool,
    pub error: Option<String>,
}

// ---------------- 行解析 ----------------

/// JSONL 信封: 顶层 timestamp + payload
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    timestamp: Option<String>,
    // Option<T> 本身即可选, 不能加 #[serde(default)](会要求 T: Default)
    payload: Option<T>,
}

#[derive(Debug, Deserialize)]
struct EventMsgPayload {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    info: Option<TokenCountInfo>,
}

#[derive(Debug, Deserialize)]
struct TokenCountInfo {
    #[serde(default)]
    total_token_usage: Option<Usage>,
    #[serde(default)]
    last_token_usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct SessionMetaPayload {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model_provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TurnContextPayload {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// 一次调用的用量桶
#[derive(Debug, Clone, Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cached_input_tokens: Option<i64>,
    #[serde(default)]
    reasoning_output_tokens: Option<i64>,
    #[serde(default)]
    total_tokens: Option<i64>,
}

impl Usage {
    fn input(&self) -> i64 {
        self.input_tokens.unwrap_or(0)
    }
    fn output(&self) -> i64 {
        self.output_tokens.unwrap_or(0)
    }
    fn cached(&self) -> i64 {
        self.cached_input_tokens.unwrap_or(0)
    }
    fn reasoning(&self) -> i64 {
        self.reasoning_output_tokens.unwrap_or(0)
    }
}

/// 从一行中解析出的感兴趣事件
#[derive(Debug)]
enum LineEvent {
    /// 会话元信息(文件开头)
    Meta {
        session_id: Option<String>,
        cwd: Option<String>,
        provider: Option<String>,
    },
    /// 轮次上下文(携带模型名, 可能随轮次变化)
    Context {
        model: Option<String>,
        cwd: Option<String>,
    },
    /// 一次调用用量
    Tokens {
        ts: String,
        total: i64,
        usage: Usage,
    },
}

/// 解析一行。先用子串做廉价过滤(避免为体积巨大的工具输出行做完整 JSON 解析)。
fn parse_line(line: &str) -> Option<LineEvent> {
    if line.contains("\"type\":\"token_count\"") {
        let env: Envelope<EventMsgPayload> = serde_json::from_str(line).ok()?;
        let p = env.payload?;
        if p.kind.as_deref() != Some("token_count") {
            return None; // 子串出现在文本字段里的误命中
        }
        let info = p.info?;
        let total = info.total_token_usage.as_ref()?.total_tokens?;
        let usage = info.last_token_usage?;
        return Some(LineEvent::Tokens {
            ts: env.timestamp.unwrap_or_default(),
            total,
            usage,
        });
    }
    if line.contains("\"type\":\"session_meta\"") {
        let env: Envelope<SessionMetaPayload> = serde_json::from_str(line).ok()?;
        let p = env.payload?;
        return Some(LineEvent::Meta {
            session_id: p.session_id,
            cwd: p.cwd,
            provider: p.model_provider,
        });
    }
    if line.contains("\"type\":\"turn_context\"") {
        let env: Envelope<TurnContextPayload> = serde_json::from_str(line).ok()?;
        let p = env.payload?;
        return Some(LineEvent::Context {
            model: p.model,
            cwd: p.cwd,
        });
    }
    None
}

// ---------------- 水位线状态 ----------------

/// 每个文件的增量读取状态 + 从历史行继承的上下文字段。
#[derive(Default)]
pub struct FileState {
    offset: u64,
    /// 上一条 token_count 的累计值(用于识别"未增长 = 重复上报")
    prev_total: Option<i64>,
    /// 当前生效的模型/厂商/工作目录/会话(由 session_meta / turn_context 更新)
    model: Option<String>,
    provider: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
}

/// 写入一条调用记录。返回是否真正落库(零 token 的调用会被跳过, 不计入新增数)。
fn insert_call(
    conn: &Connection,
    st: &FileState,
    ts: &str,
    u: &Usage,
    stem: &str,
    total: i64,
) -> bool {
    let input = u.input();
    let output = u.output();
    if input + output <= 0 {
        return false; // 空调用(实测本机有 16 条 delta>0 但 last 全 0 的事件)
    }
    let recorded_at = if ts.is_empty() {
        chrono::Utc::now().to_rfc3339()
    } else {
        ts.to_string()
    };
    let reasoning = u.reasoning();
    let note = if reasoning > 0 {
        format!("Codex 调用(推理 {reasoning} tokens)")
    } else {
        "Codex 调用".to_string()
    };
    let rec = NewRecord {
        recorded_at,
        source: "codex".into(),
        provider_code: st.provider.clone().filter(|s| !s.is_empty()),
        model_name: st.model.clone().filter(|s| !s.is_empty()),
        session_id: st.session_id.clone().filter(|s| !s.is_empty()),
        // 累计值在同一文件内严格递增, 与文件名组合即为唯一键
        request_id: Some(format!("codex-{stem}-{total}")),
        // 注意: input_tokens 已包含 cached_input_tokens, 不可相加(与 DSH 口径相反)
        prompt_tokens: Some(input),
        // output_tokens 已包含 reasoning_output_tokens
        completion_tokens: Some(output),
        cached_tokens: Some(u.cached()),
        cost_usd: None,
        cost_source: None,
        project: st.cwd.clone().filter(|s| !s.is_empty()),
        tags: None,
        note: Some(note),
    };
    crate::domain::records::insert(conn, &rec, None).is_ok()
}

/// 递归收集 rollout-*.jsonl
fn collect_rollout_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rollout_files(&p, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                out.push(p);
            }
        }
    }
}

/// 单轮扫描: 返回本次新增条数。
///
/// 整轮包在一个事务里: 首次扫描要补录数千条历史调用(实测本机 3590 条),
/// 逐条 INSERT 各自提交会带来数千次 fsync, 明显拖慢启动并长时间占住 DB 锁。
pub fn scan_once(
    conn: &Connection,
    codex_home: &Path,
    states: &mut HashMap<PathBuf, FileState>,
) -> Result<usize, String> {
    let sessions = codex_home.join("sessions");
    if !sessions.is_dir() {
        return Err(format!("未找到 {} 目录", sessions.display()));
    }
    let mut files = Vec::new();
    collect_rollout_files(&sessions, &mut files);

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

    // 文件被截断/重建 → 重置(旧的 request_id 由唯一索引兜底去重)
    if state.offset > cur_size {
        *state = FileState::default();
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

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    // 有些会话的 turn_context 出现在首次 token_count 之后(实测本机 3 个文件如此),
    // 这些早期调用本身没有模型信息。用本段内第一个 turn_context 的模型兜底 ——
    // 同一会话的模型极少变化, 且遇到真正的 turn_context 后 state.model 会被覆盖。
    let fallback_model: Option<String> = buf
        .split(|b| *b == b'\n')
        .filter_map(|l| std::str::from_utf8(l).ok())
        .filter(|l| l.contains("\"type\":\"turn_context\""))
        .find_map(|l| {
            let env: Envelope<TurnContextPayload> = serde_json::from_str(l).ok()?;
            env.payload?.model
        });

    let mut added = 0usize;
    let mut scan_pos: usize = 0;
    while scan_pos < buf.len() {
        // 只处理以 \n 结尾的完整行; 末尾半行留给下次
        let nl = match buf[scan_pos..].iter().position(|b| *b == b'\n') {
            Some(p) => scan_pos + p,
            None => break,
        };
        let raw = &buf[scan_pos..nl];
        let end = if raw.last() == Some(&b'\r') { raw.len() - 1 } else { raw.len() };
        if let Ok(line) = std::str::from_utf8(&buf[scan_pos..scan_pos + end]) {
            match parse_line(line) {
                Some(LineEvent::Meta { session_id, cwd, provider }) => {
                    if session_id.is_some() {
                        state.session_id = session_id;
                    }
                    if provider.is_some() {
                        state.provider = provider;
                    }
                    if cwd.is_some() {
                        state.cwd = cwd;
                    }
                }
                Some(LineEvent::Context { model, cwd }) => {
                    if model.is_some() {
                        state.model = model;
                    }
                    if cwd.is_some() {
                        state.cwd = cwd;
                    }
                }
                Some(LineEvent::Tokens { ts, total, usage }) => {
                    // 累计值未增长 = 重复上报 → 跳过; 首条无条件采用
                    let grew = match state.prev_total {
                        Some(prev) => total > prev,
                        None => true,
                    };
                    if grew {
                        // 模型尚未确立时用本段首个 turn_context 兜底
                        if state.model.is_none() {
                            state.model = fallback_model.clone();
                        }
                        if insert_call(conn, state, &ts, &usage, &stem, total) {
                            added += 1;
                        }
                    }
                    state.prev_total = Some(total);
                }
                None => {}
            }
        }
        scan_pos = nl + 1;
    }
    state.offset += scan_pos as u64;
    added
}

/// 启动后台检测线程(每 poll_ms 扫描一次)。
pub fn start_watcher(
    conn: Arc<Mutex<Connection>>,
    codex_home: PathBuf,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("codex-watcher".into())
        .spawn(move || {
            let mut states: HashMap<PathBuf, FileState> = HashMap::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(guard) = conn.lock() {
                    if let Err(e) = scan_once(&guard, &codex_home, &mut states) {
                        eprintln!("[codex-watcher] {e}");
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
        .map_err(|e| format!("Codex 检测线程启动失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// 构造一条 token_count 行(格式与真实 rollout 一致: 紧凑 JSON)
    #[allow(clippy::too_many_arguments)]
    fn token_line(
        ts: &str,
        total_in: i64,
        total_out: i64,
        total_cached: i64,
        last_in: i64,
        last_out: i64,
        last_cached: i64,
        last_reason: i64,
    ) -> String {
        format!(
            concat!(
                r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"#,
                r#""total_token_usage":{{"input_tokens":{ti},"cached_input_tokens":{tc},"#,
                r#""cache_write_input_tokens":0,"output_tokens":{to},"reasoning_output_tokens":0,"total_tokens":{tt}}},"#,
                r#""last_token_usage":{{"input_tokens":{li},"cached_input_tokens":{lc},"#,
                r#""cache_write_input_tokens":0,"output_tokens":{lo},"reasoning_output_tokens":{lr},"total_tokens":{lt}}},"#,
                r#""model_context_window":258400}}}}}}"#
            ),
            ts = ts,
            ti = total_in,
            tc = total_cached,
            to = total_out,
            tt = total_in + total_out,
            li = last_in,
            lc = last_cached,
            lo = last_out,
            lr = last_reason,
            lt = last_in + last_out,
        )
    }

    fn meta_line(session: &str, cwd: &str, provider: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-03T00:49:56.000Z","type":"session_meta","payload":{{"session_id":"{s}","cwd":"{c}","originator":"codex_work_desktop","cli_version":"0.145.0","source":"vscode","model_provider":"{p}"}}}}"#,
            s = session,
            c = cwd.replace('\\', "\\\\"),
            p = provider
        )
    }

    fn ctx_line(model: &str, cwd: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-08-03T00:49:57.000Z","type":"turn_context","payload":{{"turn_id":"t1","cwd":"{c}","model":"{m}"}}}}"#,
            c = cwd.replace('\\', "\\\\"),
            m = model
        )
    }

    fn open_test_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.db")).unwrap();
        db::migrate(&conn).unwrap();
        (dir, conn)
    }

    fn count_codex(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM usage_record WHERE source='codex'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// 写入一个 rollout 文件(3 层日期目录)
    fn write_rollout(root: &Path, name: &str, content: &str) -> PathBuf {
        let dir = root.join("sessions").join("2026").join("08").join("03");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn parses_token_count_and_context() {
        let l = token_line("2026-08-03T00:50:00.000Z", 1000, 50, 800, 1000, 50, 800, 10);
        match parse_line(&l) {
            Some(LineEvent::Tokens { total, usage, .. }) => {
                assert_eq!(total, 1050);
                assert_eq!(usage.input(), 1000);
                assert_eq!(usage.cached(), 800);
                assert_eq!(usage.reasoning(), 10);
            }
            other => panic!("应解析为 Tokens, 实际 {other:?}"),
        }
        assert!(matches!(
            parse_line(&meta_line("s1", "C:\\w", "openai")),
            Some(LineEvent::Meta { .. })
        ));
        assert!(matches!(
            parse_line(&ctx_line("gpt-5.6-sol", "C:\\w")),
            Some(LineEvent::Context { .. })
        ));
        // 无关行
        assert!(parse_line(r#"{"type":"response_item","payload":{"type":"message"}}"#).is_none());
    }

    #[test]
    fn scan_inserts_with_correct_fields() {
        let (dir, conn) = open_test_db();
        let content = format!(
            "{}\n{}\n{}\n",
            meta_line("sess-1", "C:\\work", "openai"),
            ctx_line("gpt-5.6-sol", "C:\\work"),
            token_line("2026-08-03T00:50:00.000Z", 12027, 226, 11008, 12027, 226, 11008, 155),
        );
        write_rollout(dir.path(), "rollout-2026-08-03T00-49-56-sess1.jsonl", &content);

        let mut states = HashMap::new();
        let n = scan_once(&conn, dir.path(), &mut states).unwrap();
        assert_eq!(n, 1);

        let (model, provider, project, session, prompt, comp, cached, total): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT model_name, provider_code, project, session_id,
                        prompt_tokens, completion_tokens, cached_tokens, total_tokens
                 FROM usage_record WHERE source='codex'",
                [],
                |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                        r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(provider.as_deref(), Some("openai"));
        assert_eq!(project.as_deref(), Some("C:\\work"));
        assert_eq!(session.as_deref(), Some("sess-1"));
        // 关键口径: prompt = input(已含缓存), 不是 input + cached
        assert_eq!(prompt, 12027, "input 已包含 cached, 不可相加");
        assert_eq!(cached, 11008);
        assert_eq!(comp, 226);
        assert_eq!(total, 12253, "total = prompt + completion");
    }

    #[test]
    fn duplicate_event_without_growth_is_skipped() {
        let (dir, conn) = open_test_db();
        // 同一累计值出现两次 = 重复上报
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            meta_line("s2", "C:\\w", "openai"),
            ctx_line("gpt-5.6-sol", "C:\\w"),
            token_line("2026-08-03T00:50:00.000Z", 1000, 10, 0, 1000, 10, 0, 0),
            token_line("2026-08-03T00:50:01.000Z", 1000, 10, 0, 1000, 10, 0, 0),
        );
        write_rollout(dir.path(), "rollout-2026-08-03T00-49-56-s2.jsonl", &content);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        assert_eq!(count_codex(&conn), 1, "累计值未增长的重复事件应被跳过");
    }

    #[test]
    fn resumed_session_first_event_still_recorded() {
        let (dir, conn) = open_test_db();
        // resume 会话: 首条 total 已继承(2887 万), last 才是本次真实用量
        let content = format!(
            "{}\n{}\n{}\n",
            meta_line("s3", "C:\\w", "openai"),
            ctx_line("gpt-5.6-sol", "C:\\w"),
            token_line("2026-08-03T00:50:00.000Z", 28_877_965, 86_845, 0, 65_923, 676, 0, 0),
        );
        write_rollout(dir.path(), "rollout-2026-08-03T00-49-56-s3.jsonl", &content);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        let prompt: i64 = conn
            .query_row(
                "SELECT prompt_tokens FROM usage_record WHERE source='codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prompt, 65_923, "应取 last 而非继承的 total");
    }

    #[test]
    fn incremental_read_only_adds_new_events() {
        let (dir, conn) = open_test_db();
        let base = format!(
            "{}\n{}\n",
            meta_line("s4", "C:\\w", "openai"),
            ctx_line("gpt-5.6-sol", "C:\\w"),
        );
        let f = write_rollout(
            dir.path(),
            "rollout-2026-08-03T00-49-56-s4.jsonl",
            &format!("{base}{}\n", token_line("2026-08-03T00:50:00.000Z", 100, 10, 0, 100, 10, 0, 0)),
        );

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        // 无变化 → 0
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 0);

        // 追加第二次调用(累计增长)
        let mut c = std::fs::read_to_string(&f).unwrap();
        c.push_str(&format!(
            "{}\n",
            token_line("2026-08-03T00:51:00.000Z", 300, 25, 0, 200, 15, 0, 0)
        ));
        std::fs::write(&f, c).unwrap();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        assert_eq!(count_codex(&conn), 2);
    }

    #[test]
    fn model_change_mid_session_is_followed() {
        let (dir, conn) = open_test_db();
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            meta_line("s5", "C:\\w", "openai"),
            ctx_line("gpt-5.6-sol", "C:\\w"),
            token_line("2026-08-03T00:50:00.000Z", 100, 10, 0, 100, 10, 0, 0),
            ctx_line("gpt-5.6-mini", "C:\\w"),
            token_line("2026-08-03T00:51:00.000Z", 300, 30, 0, 200, 20, 0, 0),
        );
        write_rollout(dir.path(), "rollout-2026-08-03T00-49-56-s5.jsonl", &content);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 2);
        let models: Vec<String> = conn
            .prepare("SELECT model_name FROM usage_record WHERE source='codex' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(models, vec!["gpt-5.6-sol", "gpt-5.6-mini"]);
    }

    #[test]
    fn ignores_non_rollout_files() {
        let (dir, conn) = open_test_db();
        write_rollout(dir.path(), "other.jsonl", "{\"type\":\"session_meta\"}\n");
        write_rollout(dir.path(), "notes.txt", "noise");
        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 0);
    }

    /// 回归: turn_context 出现在首次 token_count 之后时, 早期调用仍应拿到模型
    /// (实测本机有 3 个 rollout 文件是这种顺序, 曾导致 166 条记录模型为空)
    #[test]
    fn model_backfilled_when_turn_context_comes_late() {
        let (dir, conn) = open_test_db();
        let content = format!(
            "{}\n{}\n{}\n",
            meta_line("s6", "C:\\w", "openai"),
            // 注意顺序: 先有调用, 后有 turn_context
            token_line("2026-08-01T21:34:30.000Z", 100, 10, 0, 100, 10, 0, 0),
            ctx_line("gpt-5.6-sol", "C:\\w"),
        );
        write_rollout(dir.path(), "rollout-2026-08-01T21-34-26-s6.jsonl", &content);

        let mut states = HashMap::new();
        assert_eq!(scan_once(&conn, dir.path(), &mut states).unwrap(), 1);
        let model: Option<String> = conn
            .query_row(
                "SELECT model_name FROM usage_record WHERE source='codex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            model.as_deref(),
            Some("gpt-5.6-sol"),
            "晚到的 turn_context 应兜底补齐早期调用的模型"
        );
    }

    /// 端到端(手动触发): 扫描真实 `CODEX_HOME`(默认 `~/.codex`)到**临时库**,
    /// 核对调用数/用量总量/"模型为空"条数。不触碰应用数据库。
    /// 运行: cargo test --lib scan_real_codex_home -- --ignored --nocapture
    #[test]
    #[ignore]
    fn scan_real_codex_home() {
        let home = match std::env::var("CODEX_HOME") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                let base = std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .ok();
                match base {
                    Some(b) => std::path::PathBuf::from(b).join(".codex"),
                    None => return,
                }
            }
        };
        if !home.join("sessions").is_dir() {
            eprintln!("[codex-e2e] skip: {} 不存在", home.join("sessions").display());
            return;
        }

        let (_dir, conn) = open_test_db();
        let mut states = HashMap::new();
        let n = scan_once(&conn, &home, &mut states).unwrap();

        let (calls, tin, tout, tcached, no_model): (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(prompt_tokens),0),
                        COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(cached_tokens),0),
                        SUM(CASE WHEN model_name IS NULL OR model_name='' THEN 1 ELSE 0 END)
                 FROM usage_record WHERE source='codex'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        eprintln!("[codex-e2e] 首次扫描新增 = {n}");
        eprintln!("[codex-e2e] 调用={calls} 输入={tin} 输出={tout} 缓存={tcached}");
        eprintln!("[codex-e2e] 模型为空 = {no_model} 条");

        // 偏移已推进, 第二轮不应重复入库
        assert_eq!(scan_once(&conn, &home, &mut states).unwrap(), 0, "第二轮不应新增");
        assert!(calls > 0, "应至少入库一条");
        assert_eq!(no_model, 0, "模型为空(TurnContext 晚于调用时应已兜底)");
    }
}
