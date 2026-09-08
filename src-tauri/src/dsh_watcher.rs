//! DSH(DeepSeek Harness) 本机用量自动检测器。
//!
//! DSH 会把每个会话的实时 token 用量快照写入:
//!   `~/.dsh/storages/session_projcache/sessions/<session-id>.json`
//! 其中 `record.rows.tokenUsage.val` 形如:
//! ```json
//! {
//!   "totals": {"uncachedInputTokens":..,"outputTokens":..,"cacheReadTokens":..,"cacheWriteTokens":..},
//!   "last": {"turn":14,"step":3,"buckets":{"uncachedInputTokens":212,"outputTokens":232,
//!            "cacheReadTokens":419200,"cacheWriteTokens":0}}
//! }
//! ```
//! `last.turn/step` 是最近一次 LLM 调用的位置。检测器记住每个会话的
//! (turn, step) 水位; 发现新水位出现时, 把 `last.buckets` 作为一次新调用
//! 入库(recorded_at 用文件 mtime, source='dsh')。
//!
//! 语义: uncachedInputTokens=未命中缓存输入, cacheReadTokens=缓存命中输入,
//! outputTokens=输出。入库时 prompt=uncached+cached, cached 单列供分开计价。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Deserialize;

use crate::domain::records::NewRecord;

/// 默认轮询间隔(ms)
pub const DEFAULT_POLL_MS: u64 = 3000;

fn relative_projcache(dsh_home: &Path) -> PathBuf {
    dsh_home.join("storages").join("session_projcache").join("sessions")
}

// ---------------- 快照 JSON 抽取结构 ----------------

#[derive(Debug, Deserialize)]
struct SessionSnapshot {
    record: RecordBlock,
}

#[derive(Debug, Deserialize)]
struct RecordBlock {
    rows: RowsBlock,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowsBlock {
    #[serde(default)]
    token_usage: Option<RowWrap<TokenUsageVal>>,
    #[serde(default)]
    model_selection: Option<RowWrap<ModelSelectionVal>>,
}

#[derive(Debug, Deserialize)]
struct RowWrap<T> {
    val: T,
}

#[derive(Debug, Deserialize)]
struct TokenUsageVal {
    last: Option<LastCall>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct TokenBuckets {
    uncached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
}

impl TokenBuckets {
    fn uncached(&self) -> i64 {
        self.uncached_input_tokens.unwrap_or(0)
    }
    fn output(&self) -> i64 {
        self.output_tokens.unwrap_or(0)
    }
    fn cached(&self) -> i64 {
        self.cache_read_tokens.unwrap_or(0)
    }
}

#[derive(Debug, Deserialize, Clone)]
struct LastCall {
    turn: Option<i64>,
    step: Option<i64>,
    #[serde(default)]
    buckets: Option<TokenBuckets>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelSelectionVal {
    #[serde(default)]
    last_used: Option<LastUsedModel>,
}

#[derive(Debug, Deserialize, Clone)]
struct LastUsedModel {
    provider: Option<String>,
    model: Option<String>,
}

/// 一次会话快照的解析结果
#[derive(Debug)]
struct ParsedSession {
    session_id: String,
    file_mtime_ms: i64,
    turn: i64,
    step: i64,
    buckets: TokenBuckets,
    model: Option<String>,
    provider: Option<String>,
    workspace: Option<String>,
}

fn parse_snapshot(path: &Path, content: &[u8]) -> Option<ParsedSession> {
    let snap: SessionSnapshot = serde_json::from_slice(content).ok()?;
    let tu = snap.record.rows.token_usage?;
    let tu = tu.val;
    let last = tu.last?;
    let turn = last.turn?;
    let step = last.step?;
    let buckets = last.buckets.clone()?;
    if buckets.uncached() == 0 && buckets.output() == 0 && buckets.cached() == 0 {
        return None; // 空调用(如纯工具步骤)
    }
    let file_mtime_ms = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .trim_start_matches("session-")
        .to_string();
    let workspace = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.trim_matches('-').to_string());
    let msel = snap
        .record
        .rows
        .model_selection
        .as_ref()
        .and_then(|m| m.val.last_used.clone());

    Some(ParsedSession {
        session_id,
        file_mtime_ms,
        turn,
        step,
        buckets,
        model: msel.as_ref().and_then(|m| m.model.clone()),
        provider: msel.as_ref().and_then(|m| m.provider.clone()),
        workspace,
    })
}

/// provider 别名 → 价格库 code(deepseek-official → deepseek 等)
fn map_provider(p: Option<&str>) -> Option<String> {
    let p = p?.trim().to_lowercase();
    if p.contains("deepseek") {
        Some("deepseek".to_string())
    } else if p.contains("moonshot") || p.contains("kimi") {
        Some("kimi".to_string())
    } else if p.contains("anthropic") || p.contains("claude") {
        Some("anthropic".to_string())
    } else if p.contains("openai") || p.contains("gpt") {
        Some("openai".to_string())
    } else if !p.is_empty() {
        Some(p)
    } else {
        None
    }
}

fn insert_call(conn: &Connection, ps: &ParsedSession) {
    let uncached = ps.buckets.uncached();
    let cached = ps.buckets.cached();
    let output = ps.buckets.output();
    if uncached + cached + output <= 0 {
        return;
    }
    let recorded_at = chrono::DateTime::from_timestamp_millis(ps.file_mtime_ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let rec = NewRecord {
        recorded_at,
        source: "dsh".into(),
        provider_code: map_provider(ps.provider.as_deref()),
        model_name: ps.model.clone(),
        session_id: Some(ps.session_id.clone()),
        request_id: Some(format!("dsh-{}-{}-{}", ps.session_id, ps.turn, ps.step)),
        prompt_tokens: Some(uncached + cached),
        completion_tokens: Some(output),
        cached_tokens: Some(cached),
        cost_usd: None,
        cost_source: None,
        project: ps.workspace.clone(),
        tags: None,
        note: Some(format!("DSH turn {}/step {}", ps.turn, ps.step)),
    };
    let _ = crate::domain::records::insert(conn, &rec, None);
}

type Watermark = HashMap<String, (i64, i64)>;

/// 单轮扫描: 新水位出现即入库。返回本次新增条数。
pub fn scan_once(
    conn: &Connection,
    dsh_home: &Path,
    watermark: &mut Watermark,
) -> Result<usize, String> {
    let dir = relative_projcache(dsh_home);
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("读取 {} 失败: {e}", dir.display()))?;
    let mut added = 0usize;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read(&path) else { continue };
        let Some(ps) = parse_snapshot(&path, &content) else { continue };
        let key = format!(
            "{}|{}",
            ps.workspace.as_deref().unwrap_or(""),
            ps.session_id
        );
        let cur = (ps.turn, ps.step);
        let is_new = match watermark.get(&key) {
            Some(prev) => *prev != cur,
            None => true,
        };
        if !is_new {
            continue;
        }
        watermark.insert(key, cur);
        insert_call(conn, &ps);
        added += 1;
    }
    Ok(added)
}

/// 启动后台检测线程(每 poll_ms 扫描一次)。
pub fn start_watcher(
    conn: Arc<Mutex<Connection>>,
    dsh_home: PathBuf,
    poll_ms: u64,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("dsh-watcher".into())
        .spawn(move || {
            let mut watermark: Watermark = Watermark::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(guard) = conn.lock() {
                    if let Err(e) = scan_once(&guard, &dsh_home, &mut watermark) {
                        eprintln!("[dsh-watcher] {e}");
                    }
                }
                let mut waited = 0u64;
                while waited < poll_ms {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    waited += 200;
                }
            }
        })
        .map_err(|e| format!("DSH 检测线程启动失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json(turn: i64, step: i64, uncached: i64, out: i64, cached: i64) -> String {
        format!(
            r#"{{
              "version": 5,
              "record": {{
                "rows": {{
                  "modelSelection": {{ "val": {{ "lastUsed": {{ "provider": "deepseek-official", "model": "deepseek-v4-flash" }} }} }},
                  "tokenUsage": {{
                    "val": {{
                      "totals": {{ "uncachedInputTokens": 100, "outputTokens": 100, "cacheReadTokens": 10, "cacheWriteTokens": 0 }},
                      "last": {{ "turn": {turn}, "step": {step}, "buckets": {{
                        "uncachedInputTokens": {uncached}, "outputTokens": {out},
                        "cacheReadTokens": {cached}, "cacheWriteTokens": 0
                      }} }}
                    }}
                  }}
                }}
              }}
            }}"#
        )
    }

    #[test]
    fn parse_works_and_ignores_empty_calls() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("proj");
        std::fs::create_dir_all(&sessions).unwrap();
        let f = sessions.join("session-abc123.json");
        std::fs::write(&f, sample_json(3, 1, 100, 50, 20)).unwrap();
        let ps = parse_snapshot(&f, &std::fs::read(&f).unwrap()).expect("parse");
        assert_eq!(ps.session_id, "abc123");
        assert_eq!((ps.turn, ps.step), (3, 1));
        assert_eq!(ps.buckets.uncached(), 100);
        assert_eq!(ps.buckets.output(), 50);
        assert_eq!(ps.model.as_deref(), Some("deepseek-v4-flash"));

        let f2 = sessions.join("session-empty.json");
        std::fs::write(&f2, sample_json(4, 0, 0, 0, 0)).unwrap();
        assert!(parse_snapshot(&f2, &std::fs::read(&f2).unwrap()).is_none());
    }

    #[test]
    fn scan_adds_increments_only() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.db")).unwrap();
        crate::db::migrate(&conn).unwrap();
        let dsh = dir.path().join("dsh");
        let sessions = dsh
            .join("storages")
            .join("session_projcache")
            .join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let f = sessions.join("session-s1.json");

        std::fs::write(&f, sample_json(1, 0, 1000, 500, 100)).unwrap();
        let mut wm = Watermark::new();
        let n1 = scan_once(&conn, &dsh, &mut wm).unwrap();
        assert_eq!(n1, 1, "首次快照入库");
        let (count, prompt, comp, cached): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0)
                 FROM usage_record WHERE source='dsh'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(prompt, 1100, "prompt = uncached + cached");
        assert_eq!(comp, 500);
        assert_eq!(cached, 100);

        // 同水位不重复
        let n2 = scan_once(&conn, &dsh, &mut wm).unwrap();
        assert_eq!(n2, 0);

        // 新 turn/step 增量入库
        std::fs::write(&f, sample_json(1, 1, 200, 60, 30)).unwrap();
        let n3 = scan_once(&conn, &dsh, &mut wm).unwrap();
        assert_eq!(n3, 1);
        let total_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_record WHERE source='dsh'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_rows, 2);
    }

    #[test]
    fn provider_alias_mapping() {
        assert_eq!(map_provider(Some("deepseek-official")), Some("deepseek".to_string()));
        assert_eq!(map_provider(Some("DeepSeek Platform")), Some("deepseek".to_string()));
        assert_eq!(map_provider(None), None);
        assert_eq!(map_provider(Some("moonshot")), Some("kimi".to_string()));
    }
}
