//! Hermes Agent 本机用量自动检测器。
//!
//! Hermes Agent(NousResearch)把会话与用量持久化在 **SQLite**:
//!   - Windows 默认 `%LOCALAPPDATA%\hermes\state.db`
//!   - macOS / Linux 默认 `~/.hermes/state.db`
//!   - 可用 `HERMES_HOME` 环境变量覆盖(命名 profile 是独立目录:
//!     `<hermes home>/profiles/<name>/state.db`, 本检测器会一并扫描)
//!   - **免安装(portable)版**把 home 放在安装目录下
//!     (`<免安装根目录>\data\hermes-home`), 既没有注册表项也没有快捷方式线索,
//!     所以额外从**正在运行的 Hermes 进程**路径反推(见 [`discover_running_homes`]);
//!     也可以在设置页显式指定(kv_settings.hermes_home; 填免安装根目录、
//!     `...\data\hermes-home` 或 `state.db` 文件路径都可以, 见 [`normalise_home`])。
//!
//! ## 为什么是"水位线取增量"
//!
//! 与 Claude Code / Codex / WorkBuddy 的逐次调用 JSONL 不同, Hermes 存的是
//! **按 (会话, 模型, 厂商) 聚合的累计计数**, 没有逐次调用明细:
//!   - 新版本: `session_model_usage` 表(按 session/model/provider/base_url/mode/task 聚合,
//!     PK 含 task; 本检测器把同组的多行求和);
//!   - 旧版本没有该表时退化为 `sessions` 表的会话总量(按会话的 model 归属)。
//!
//! 所以本检测器:
//!   1. 读取每个 Hermes 库的累计计数(只读打开, 绝不在 Hermes 库里写任何东西);
//!   2. 与应用库 `source_cursor` 里上次已入库的值相减得到差额;
//!   3. 差额入库一条 `source='hermes'` 记录, 并把水位线推进到新值。
//!
//! 水位线持久化在应用库里, 因此**重启不会重复计数**; 应用未运行期间 Hermes 产生的
//! 用量会在下次扫描时作为一条增量补齐(时间戳取 Hermes 的 `last_seen`, 回退
//! `started_at`/`ended_at`, 再回退当前时间), 不会把历史用量堆进"今天"。
//!
//! 同一会话如果 smu 存在但某行全部为 0, 该会话不会再被总量兜底重复计入
//! (与主流做法一致): 只要一个会话**有任何** smu 行, 就信任 smu。
//!
//! ## 口径(与 Hermes 自己的总用量一致)
//!
//! Hermes 的五个计数是**并列相加**的桶 —— `input_tokens` 不含缓存,
//! `reasoning_tokens` 独立于 `output_tokens`(Hermes 的迁移脚本也用五者之和判断
//! 会话是否有用量):
//! ```text
//! prompt_tokens     = input + cache_read + cache_write
//! completion_tokens = output + reasoning   (两者都按输出计价)
//! cached_tokens     = cache_read
//! ```
//!
//! 费用: 单价库里有该模型的价格时按本应用单价估算(与其它通道口径一致, 也不会
//! 覆盖用户自定义单价); 单价库里没有时回退 Hermes 自报的
//! `actual_cost_usd`(标记 official)或 `estimated_cost_usd`(标记 computed)。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::domain::records::NewRecord;

/// 默认轮询间隔(ms)
pub const DEFAULT_POLL_MS: u64 = 3000;

/// 设置页里"Hermes 数据目录"对应的 kv_settings 键。
/// 留空 = 自动探测; 也可显式指定(免安装根目录 / `data\hermes-home` / `state.db` 都行)。
pub const HOME_SETTING_KEY: &str = "hermes_home";

/// 启动结果(供前端展示)。
#[derive(Clone, serde::Serialize)]
pub struct WatcherInfo {
    pub hermes_home: String,
    /// 主库路径(多 profile 时还会有 profiles/<name>/state.db)
    pub state_db: String,
    pub started: bool,
    pub error: Option<String>,
}

// ---------------- Hermes home / 库文件定位 ----------------

/// `HERMES_HOME` 环境变量(Hermes 自己解析 home 的第一优先级)。
fn env_home() -> Option<PathBuf> {
    let v = std::env::var("HERMES_HOME").ok()?;
    let p = PathBuf::from(v);
    if p.as_os_str().is_empty() {
        None
    } else {
        Some(p)
    }
}

/// 平台默认 home: Windows `%LOCALAPPDATA%\hermes`, 其它平台 `~/.hermes`。
fn platform_default_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(v) = std::env::var("LOCALAPPDATA") {
            return Some(PathBuf::from(v).join("hermes"));
        }
    }
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|h| PathBuf::from(h).join(".hermes"))
}

/// 静态解析 Hermes home: `HERMES_HOME` → 平台默认。
///
/// 注意免安装(portable)版把 home 放在安装目录里, 这里探测不到 ——
/// 由 [`discover_running_homes`] 从其运行中的进程路径反推。
pub fn resolve_home() -> Option<PathBuf> {
    env_home().or_else(platform_default_home)
}

/// 从某个可执行文件路径向上找 Hermes home(免安装版布局):
/// `<root>\Hermes Agent CN Desktop.exe` → `<root>\data\hermes-home`;
/// 运行时子进程在 `<root>\data\versions\<ver>\...` 深处, 所以逐级上溯。
/// 只认真正含 `state.db` 的目录, 避免把无关目录当 home。
fn home_from_exe(exe: &Path) -> Option<PathBuf> {
    let mut dir = exe.parent();
    let mut hops = 0;
    while let Some(d) = dir {
        for cand in [d.join("data").join("hermes-home"), d.join("hermes-home")] {
            if cand.join("state.db").is_file() {
                return Some(cand);
            }
        }
        hops += 1;
        if hops >= 6 {
            break; // 免安装根目录就在 exe 上方几级, 不必上溯到盘根
        }
        dir = d.parent();
    }
    None
}

/// 探测本机**正在运行**的 Hermes 进程, 反推其数据目录(免安装版唯一可行的自动识别方式:
/// 既没有注册表项也没有快捷方式)。返回去重后的 home 列表。
pub fn discover_running_homes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for exe in process_image_paths() {
        let name = exe
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !name.contains("hermes") {
            continue;
        }
        if let Some(h) = home_from_exe(&exe) {
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
    out.sort();
    out
}

/// 列出当前所有进程的可执行文件全路径(Windows; 其它平台返回空)。
#[cfg(windows)]
fn process_image_paths() -> Vec<PathBuf> {
    win_process::image_paths()
}

#[cfg(not(windows))]
fn process_image_paths() -> Vec<PathBuf> {
    Vec::new()
}

/// 极小的 Win32 进程枚举(不引入额外 crate):
/// `EnumProcesses` 拿 PID → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
/// → `QueryFullProcessImageNameW` 拿全路径。失败/无权限的进程直接跳过。
#[cfg(windows)]
mod win_process {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    type Handle = *mut core::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn QueryFullProcessImageNameW(h: Handle, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }

    #[link(name = "psapi")]
    extern "system" {
        fn EnumProcesses(pids: *mut u32, cb: u32, needed: *mut u32) -> i32;
    }

    fn image_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 || len == 0 {
                return None;
            }
            Some(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
        }
    }

    pub fn image_paths() -> Vec<PathBuf> {
        let mut cap = 1024usize;
        loop {
            let mut pids = vec![0u32; cap];
            let mut needed = 0u32;
            let ok = unsafe {
                EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed)
            };
            if ok == 0 {
                return Vec::new();
            }
            let count = (needed / 4) as usize;
            if count >= cap && cap < 16384 {
                cap *= 2;
                continue;
            }
            pids.truncate(count);
            return pids
                .into_iter()
                .filter(|p| *p != 0)
                .filter_map(image_path)
                .collect();
        }
    }
}

/// 进程探测有成本, 30 秒内复用结果; 结果只增不减(进程退出后其库文件仍在,
/// 依旧可以继续读, 不必把已识别的目录丢掉)。
fn discovered_homes_cached() -> Vec<PathBuf> {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    const TTL: std::time::Duration = std::time::Duration::from_secs(30);
    static CACHE: OnceLock<Mutex<(Option<Instant>, Vec<PathBuf>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((None, Vec::new())));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let stale = guard.0.map_or(true, |t| t.elapsed() >= TTL);
    if stale {
        for h in discover_running_homes() {
            if !guard.1.contains(&h) {
                guard.1.push(h);
            }
        }
        guard.0 = Some(Instant::now());
    }
    guard.1.clone()
}

/// 归一化用户填写的路径, 三种写法都接受:
/// - `<home>\state.db`(直接指向库文件)→ 取所在目录
/// - 免安装版根目录(含 `data\hermes-home`)→ 取 `data\hermes-home`
/// - 普通 hermes home / 含 `hermes-home` 子目录 → 原样
pub fn normalise_home(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw.trim());
    if p.is_file() {
        if let Some(parent) = p.parent() {
            if parent.as_os_str().is_empty() {
                return PathBuf::from(".");
            }
            return parent.to_path_buf();
        }
    }
    let portable = p.join("data").join("hermes-home");
    if portable.is_dir() {
        return portable;
    }
    let nested = p.join("hermes-home");
    if nested.is_dir() {
        return nested;
    }
    p
}

/// 列出待扫描的 Hermes 库: 主库 + 各命名 profile 的库(只保留实际存在的文件)。
pub fn resolve_dbs(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let main = home.join("state.db");
    if main.is_file() {
        out.push(main);
    }
    let profiles = home.join("profiles");
    if let Ok(entries) = std::fs::read_dir(&profiles) {
        let mut extra: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("state.db"))
            .filter(|p| p.is_file())
            .collect();
        extra.sort();
        out.extend(extra);
    }
    out
}

/// 当前生效的 Hermes 配置(设置页改动后下一次轮询即生效)。
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// 本次参与扫描的 home(可能多个: 默认 home + 自动识别到的免安装目录)
    pub homes: Vec<PathBuf>,
    pub dbs: Vec<PathBuf>,
    pub error: Option<String>,
}

/// 读设置 + 定位实际存在的库。
///
/// 优先级:
/// 1. 设置页显式指定的目录(唯一权威, 不再猜);
/// 2. `HERMES_HOME` 环境变量(Hermes 自己解析 home 的第一优先级);
/// 3. 平台默认 home + 从运行中的 Hermes 进程反推出来的免安装目录(两者合并)。
pub fn load_config(conn: &Connection) -> Config {
    build_config(conn, discovered_homes_cached())
}

/// [`load_config`] 的纯逻辑部分(discovered 由调用方注入, 便于测试)。
fn build_config(conn: &Connection, discovered: Vec<PathBuf>) -> Config {
    let explicit = conn
        .query_row(
            "SELECT value FROM kv_settings WHERE key = ?1",
            [HOME_SETTING_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let homes: Vec<PathBuf> = if let Some(raw) = explicit {
        vec![normalise_home(&raw)]
    } else if let Some(env) = env_home() {
        vec![env]
    } else {
        let default = platform_default_home();
        let mut v: Vec<PathBuf> = Vec::new();
        if let Some(d) = default.clone() {
            v.push(d);
        }
        for h in discovered {
            if !v.contains(&h) {
                v.push(h);
            }
        }
        // 静态默认目录里没有库时, 别让它挡住自动识别出来的目录
        let with_db: Vec<PathBuf> = v.iter().filter(|h| !resolve_dbs(h).is_empty()).cloned().collect();
        if with_db.is_empty() {
            // 一个都没找到: 保留默认路径, 至少能给用户一个明确的提示
            default.into_iter().collect()
        } else {
            with_db
        }
    };

    let mut dbs: Vec<PathBuf> = Vec::new();
    for h in &homes {
        dbs.extend(resolve_dbs(h));
    }
    dbs.sort();
    dbs.dedup();

    let error = if dbs.is_empty() {
        let where_ = homes
            .iter()
            .map(|h| h.display().to_string())
            .collect::<Vec<_>>()
            .join("、");
        Some(format!("{where_} 下没有找到 state.db"))
    } else {
        None
    };

    Config { homes, dbs, error }
}

/// 组装给前端的状态信息。
pub fn info_from(cfg: &Config, started: bool) -> WatcherInfo {
    let homes = cfg
        .homes
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("、");
    WatcherInfo {
        hermes_home: homes,
        state_db: cfg
            .dbs
            .first()
            .map(|p| p.display().to_string())
            .or_else(|| {
                cfg.homes
                    .first()
                    .map(|p| p.join("state.db").display().to_string())
            })
            .unwrap_or_default(),
        started,
        error: cfg.error.clone(),
    }
}

// ---------------- 读 Hermes 库 ----------------

/// 一条 (会话,模型,厂商) 的累计用量。
#[derive(Debug, Clone, Default)]
struct HermesUsage {
    session_id: String,
    model: String,
    provider: String,
    /// epoch 秒
    last_seen: Option<f64>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    /// 费用(USD): actual 优先, 否则 estimated
    cost: f64,
    /// 自报的实际费用是否 > 0(决定 cost_source)
    actual_cost: f64,
    cwd: Option<String>,
}

/// 新版本: per-model 归因表(同组的多个 task/base_url 行在此求和)。
/// 列序与 `SESSION_TOTALS_SQL` 必须完全一致, 见 `decode_row`。
const PER_MODEL_SQL: &str = r#"
SELECT smu.session_id,
       COALESCE(smu.model, '')            AS model,
       COALESCE(smu.billing_provider, '') AS provider,
       MAX(COALESCE(NULLIF(smu.last_seen, 0), s.started_at)) AS last_seen,
       SUM(COALESCE(smu.input_tokens, 0)),
       SUM(COALESCE(smu.output_tokens, 0)),
       SUM(COALESCE(smu.cache_read_tokens, 0)),
       SUM(COALESCE(smu.cache_write_tokens, 0)),
       SUM(COALESCE(smu.reasoning_tokens, 0)),
       SUM(COALESCE(NULLIF(smu.actual_cost_usd, 0), smu.estimated_cost_usd, 0)) AS cost,
       SUM(COALESCE(smu.actual_cost_usd, 0)) AS actual_cost,
       MAX(s.cwd)                          AS cwd
  FROM session_model_usage smu
  JOIN sessions s ON s.id = smu.session_id
 GROUP BY smu.session_id, COALESCE(smu.model, ''), COALESCE(smu.billing_provider, '')
HAVING SUM(COALESCE(smu.input_tokens, 0))
     + SUM(COALESCE(smu.output_tokens, 0))
     + SUM(COALESCE(smu.cache_read_tokens, 0))
     + SUM(COALESCE(smu.cache_write_tokens, 0))
     + SUM(COALESCE(smu.reasoning_tokens, 0))
     + SUM(COALESCE(NULLIF(smu.actual_cost_usd, 0), smu.estimated_cost_usd, 0)) > 0
"#;

/// 旧版本兜底: 会话总量(无 per-model 拆分, 也无 cwd)。
const SESSION_TOTALS_SQL: &str = r#"
SELECT id,
       COALESCE(model, '')                AS model,
       COALESCE(billing_provider, '')     AS provider,
       COALESCE(NULLIF(ended_at, 0), started_at) AS last_seen,
       COALESCE(input_tokens, 0),
       COALESCE(output_tokens, 0),
       COALESCE(cache_read_tokens, 0),
       COALESCE(cache_write_tokens, 0),
       COALESCE(reasoning_tokens, 0),
       COALESCE(NULLIF(actual_cost_usd, 0), estimated_cost_usd, 0) AS cost,
       COALESCE(actual_cost_usd, 0)       AS actual_cost,
       NULL                               AS cwd
  FROM sessions
 WHERE COALESCE(input_tokens, 0)
     + COALESCE(output_tokens, 0)
     + COALESCE(cache_read_tokens, 0)
     + COALESCE(cache_write_tokens, 0)
     + COALESCE(reasoning_tokens, 0)
     + COALESCE(NULLIF(actual_cost_usd, 0), estimated_cost_usd, 0) > 0
"#;

fn decode_row(row: &rusqlite::Row) -> rusqlite::Result<HermesUsage> {
    Ok(HermesUsage {
        session_id: row.get(0)?,
        model: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        provider: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        last_seen: row.get::<_, Option<f64>>(3)?,
        input: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        output: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
        cache_read: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
        cache_write: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
        reasoning: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
        cost: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
        actual_cost: row.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
        cwd: row.get::<_, Option<String>>(11)?,
    })
}

/// 只读打开 Hermes 库。
///
/// WAL 库在写入方退出、`-shm` 被回收后只读打开可能被拒(SQLITE_CANTOPEN);
/// 此时退化为普通打开 —— 依然只执行 SELECT, 不会改动 Hermes 的数据。
fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => Ok(c),
        Err(_) => Connection::open(path),
    }
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn query_all(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<HermesUsage>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], decode_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 读一个 Hermes 库的全部累计用量。读失败返回 Err(由调用方决定是否打日志)。
fn read_db(path: &Path) -> Result<Vec<HermesUsage>, String> {
    let conn = open_readonly(path).map_err(|e| format!("{} 打开失败: {e}", path.display()))?;
    let _ = conn.busy_timeout(Duration::from_millis(1000));

    // per-model 表存在且可查询 → 信任它; 查询失败(旧 schema/列缺失)则整体退回会话总量,
    // 粗一点但不会因为一列缺失就丢掉所有数据。
    let mut covered: HashSet<String> = HashSet::new();
    let mut smu_ok = false;
    let mut out: Vec<HermesUsage> = Vec::new();
    if has_table(&conn, "session_model_usage") {
        match query_all(&conn, PER_MODEL_SQL) {
            Ok(rows) => {
                smu_ok = true;
                for u in rows {
                    covered.insert(u.session_id.clone());
                    out.push(u);
                }
            }
            Err(e) => eprintln!("[hermes-watcher] {} per-model 查询失败, 退回会话总量: {e}", path.display()),
        }
    }

    match query_all(&conn, SESSION_TOTALS_SQL) {
        Ok(rows) => {
            for u in rows {
                // 有 smu 行的会话不再用会话总量兜底, 否则会把同一份用量记两遍。
                if smu_ok && covered.contains(&u.session_id) {
                    continue;
                }
                out.push(u);
            }
        }
        Err(e) => return Err(format!("{} 读取会话总量失败: {e}", path.display())),
    }
    Ok(out)
}

// ---------------- 水位线与增量 ----------------

/// 上次入库的累计值。
#[derive(Debug, Clone, Default, PartialEq)]
struct Cursor {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    cost: f64,
}

impl HermesUsage {
    fn cursor(&self) -> Cursor {
        Cursor {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            reasoning: self.reasoning,
            cost: self.cost,
        }
    }
}

fn group_key(u: &HermesUsage) -> String {
    format!("{}|{}|{}", u.session_id, u.model, u.provider)
}

fn empty_to_none(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn ts_to_rfc3339(ts: Option<f64>) -> Option<String> {
    let secs = ts?;
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    let ms = (secs * 1000.0).round();
    if !ms.is_finite() || ms > i64::MAX as f64 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn load_cursors(conn: &Connection) -> rusqlite::Result<HashMap<(String, String), Cursor>> {
    let mut stmt = conn.prepare(
        "SELECT scope, group_key, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, reasoning_tokens, cost_usd
           FROM source_cursor WHERE source = 'hermes'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
            Cursor {
                input: r.get(2)?,
                output: r.get(3)?,
                cache_read: r.get(4)?,
                cache_write: r.get(5)?,
                reasoning: r.get(6)?,
                cost: r.get(7)?,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let (k, v) = r?;
        out.insert(k, v);
    }
    Ok(out)
}

fn save_cursor(
    conn: &Connection,
    scope: &str,
    key: &str,
    c: &Cursor,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO source_cursor
             (source, scope, group_key, input_tokens, output_tokens, cache_read_tokens,
              cache_write_tokens, reasoning_tokens, cost_usd, updated_at)
         VALUES ('hermes', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(source, scope, group_key) DO UPDATE SET
             input_tokens       = excluded.input_tokens,
             output_tokens      = excluded.output_tokens,
             cache_read_tokens  = excluded.cache_read_tokens,
             cache_write_tokens = excluded.cache_write_tokens,
             reasoning_tokens   = excluded.reasoning_tokens,
             cost_usd           = excluded.cost_usd,
             updated_at         = excluded.updated_at",
        rusqlite::params![
            scope,
            key,
            c.input,
            c.output,
            c.cache_read,
            c.cache_write,
            c.reasoning,
            c.cost,
            now_iso(),
        ],
    )?;
    Ok(())
}

/// 把一条差额写进 `usage_record`。返回 true 表示确实新增(未命中 request_id 去重)。
fn insert_delta(
    conn: &Connection,
    scope: &str,
    u: &HermesUsage,
    d: &Cursor,
    first_seen: bool,
) -> rusqlite::Result<bool> {
    // Hermes 五桶 → 本应用两段式口径
    let prompt = d.input + d.cache_read + d.cache_write;
    let completion = d.output + d.reasoning;
    let cached = d.cache_read;

    let model = empty_to_none(&u.model);
    let provider = empty_to_none(&u.provider);

    // 单价库有该模型 → 交给 insert 按本应用单价估算(cost_source=computed),
    // 与其它检测通道口径一致, 也不覆盖用户自定义单价; 没有才回退 Hermes 自报费用。
    let (cost_usd, cost_source) = if d.cost > 0.0 {
        let priced = match &model {
            Some(m) => crate::domain::price::price_for_model_any(conn, m)?.is_some(),
            None => false,
        };
        if priced {
            (None, None)
        } else if u.actual_cost > 0.0 {
            (Some(d.cost), Some("official".to_string()))
        } else {
            (Some(d.cost), Some("computed".to_string()))
        }
    } else {
        (None, None)
    };

    let mut note = if first_seen {
        "Hermes Agent 历史补录".to_string()
    } else {
        "Hermes Agent 增量".to_string()
    };
    if d.reasoning > 0 {
        note.push_str(&format!(" · 推理 {} tokens", d.reasoning));
    }

    let rec = NewRecord {
        recorded_at: ts_to_rfc3339(u.last_seen).unwrap_or_else(now_iso),
        source: "hermes".into(),
        provider_code: provider,
        model_name: model,
        session_id: empty_to_none(&u.session_id),
        // 累计值指纹: 同一状态重复扫描会算出同一个 request_id, 配合 cursor 双保险去重
        request_id: Some(format!(
            "hermes|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:.6}",
            scope,
            u.session_id,
            u.model,
            u.provider,
            u.input,
            u.output,
            u.cache_read,
            u.cache_write,
            u.reasoning,
            u.cost
        )),
        prompt_tokens: Some(prompt),
        completion_tokens: Some(completion),
        cached_tokens: Some(cached),
        cost_usd,
        cost_source,
        project: u.cwd.clone().and_then(|c| empty_to_none(&c)),
        tags: None,
        note: Some(note),
    };
    Ok(crate::domain::records::insert(conn, &rec, None)?.is_some())
}

/// 用应用库里的水位线把 Hermes 累计值折算成增量并入库。
fn apply_deltas(conn: &Connection, rows: Vec<(String, HermesUsage)>) -> Result<usize, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut cursors = load_cursors(conn).map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut added = 0usize;

    for (scope, u) in rows {
        let key = group_key(&u);
        let map_key = (scope.clone(), key.clone());
        let first_seen = !cursors.contains_key(&map_key);
        let prev = cursors.get(&map_key).cloned().unwrap_or_default();

        // 差额; 计数可能因 Hermes 侧会话重置/回退而变小, 负值按 0 处理(不产生负记录)
        let d = Cursor {
            input: (u.input - prev.input).max(0),
            output: (u.output - prev.output).max(0),
            cache_read: (u.cache_read - prev.cache_read).max(0),
            cache_write: (u.cache_write - prev.cache_write).max(0),
            reasoning: (u.reasoning - prev.reasoning).max(0),
            cost: (u.cost - prev.cost).max(0.0),
        };
        let d_tokens = d.input + d.output + d.cache_read + d.cache_write + d.reasoning;

        // 有新增 token, 或只是费用被补记(少见), 都要落一条记录
        if d_tokens > 0 || d.cost > 0.0 {
            if insert_delta(&tx, &scope, &u, &d, first_seen).map_err(|e| e.to_string())? {
                added += 1;
            }
        }

        // 水位线始终推进(即使本条被 request_id 去重, 也说明库里已有)
        save_cursor(&tx, &scope, &key, &u.cursor()).map_err(|e| e.to_string())?;
        cursors.insert(map_key, u.cursor());
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(added)
}

/// 单轮扫描: 读取给定 Hermes 库并入库增量, 返回新增条数。
pub fn scan_once(conn: &Connection, dbs: &[PathBuf]) -> Result<usize, String> {
    let mut rows: Vec<(String, HermesUsage)> = Vec::new();
    for db in dbs {
        if !db.is_file() {
            continue;
        }
        match read_db(db) {
            Ok(list) => {
                let scope = db.display().to_string();
                for u in list {
                    rows.push((scope.clone(), u));
                }
            }
            Err(e) => eprintln!("[hermes-watcher] {e}"),
        }
    }
    apply_deltas(conn, rows)
}

// ---------------- 后台线程 ----------------

/// 文件指纹(db 与其 -wal): 用于跳过"文件没变"的轮询, 避免每 3 秒都去查库。
type FileStamp = (u64, u64, u64, u64);

fn one_file(p: &Path) -> (u64, u64) {
    match std::fs::metadata(p) {
        Ok(m) => {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (m.len(), t)
        }
        Err(_) => (0, 0),
    }
}

fn stamp(db: &Path) -> FileStamp {
    let (dl, dt) = one_file(db);
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    let (wl, wt) = one_file(Path::new(&wal));
    (dl, dt, wl, wt)
}

/// 启动后台检测线程(每 poll_ms 扫描一次)。
///
/// 线程常驻: 每轮都重新读设置并定位库, 所以
/// - 设置页改了「Hermes 数据目录」→ 下一轮自动切换(无需重启);
/// - 后建的 `state.db` / 新增 profile / 后来才启动的 Hermes 都能被发现。
pub fn start_watcher(
    conn: Arc<Mutex<Connection>>,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("hermes-watcher".into())
        .spawn(move || {
            let mut stamps: HashMap<PathBuf, FileStamp> = HashMap::new();
            let mut cur_homes: Vec<PathBuf> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                // 只在这个短临界区里读设置/定位库, 不在持锁期间解析 Hermes 库
                let cfg = match conn.lock() {
                    Ok(guard) => load_config(&guard),
                    Err(_) => Config::default(),
                };

                if cfg.homes != cur_homes {
                    // 数据目录变了(设置页改动或新识别到免安装目录): 旧指纹作废, 重新全量核对
                    stamps.clear();
                    cur_homes = cfg.homes.clone();
                }

                let changed: Vec<PathBuf> = cfg
                    .dbs
                    .into_iter()
                    .filter(|p| {
                        let s = stamp(p);
                        if stamps.get(p) == Some(&s) {
                            false
                        } else {
                            stamps.insert(p.clone(), s);
                            true
                        }
                    })
                    .collect();

                if !changed.is_empty() {
                    // 先读 Hermes(不持有应用库锁), 再短暂持锁入库
                    if let Ok(guard) = conn.lock() {
                        if let Err(e) = scan_once(&guard, &changed) {
                            eprintln!("[hermes-watcher] {e}");
                        }
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
        .map_err(|e| format!("Hermes 检测线程启动失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// 一条 smu 行的测试输入
    #[derive(Clone)]
    struct Smu<'a> {
        session: &'a str,
        model: &'a str,
        provider: &'a str,
        /// epoch 秒
        last_seen: f64,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
        actual_cost: f64,
        estimated_cost: f64,
    }

    /// 造一个"带 session_model_usage 的新版 Hermes 库"
    fn make_smu_db(path: &Path, rows: &[Smu]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT, billing_provider TEXT,
                 started_at REAL NOT NULL, ended_at REAL, cwd TEXT,
                 input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
                 cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
                 reasoning_tokens INTEGER DEFAULT 0,
                 estimated_cost_usd REAL, actual_cost_usd REAL, message_count INTEGER DEFAULT 0);
             CREATE TABLE IF NOT EXISTS session_model_usage (
                 session_id TEXT NOT NULL, model TEXT NOT NULL,
                 billing_provider TEXT NOT NULL DEFAULT '', billing_base_url TEXT NOT NULL DEFAULT '',
                 billing_mode TEXT NOT NULL DEFAULT '', task TEXT NOT NULL DEFAULT '',
                 input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                 reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                 estimated_cost_usd REAL NOT NULL DEFAULT 0, actual_cost_usd REAL NOT NULL DEFAULT 0,
                 first_seen REAL, last_seen REAL,
                 PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task));",
        )
        .unwrap();
        for r in rows {
            conn.execute(
                "INSERT OR REPLACE INTO sessions
                     (id, source, model, billing_provider, started_at, cwd,
                      input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens)
                 VALUES (?1, 'desktop', ?2, ?3, ?4, 'C:\\work\\proj', ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    r.session,
                    r.model,
                    r.provider,
                    r.last_seen,
                    r.input,
                    r.output,
                    r.cache_read,
                    r.cache_write,
                    r.reasoning
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO session_model_usage
                     (session_id, model, billing_provider, input_tokens, output_tokens,
                      cache_read_tokens, cache_write_tokens, reasoning_tokens,
                      actual_cost_usd, estimated_cost_usd, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                rusqlite::params![
                    r.session,
                    r.model,
                    r.provider,
                    r.input,
                    r.output,
                    r.cache_read,
                    r.cache_write,
                    r.reasoning,
                    r.actual_cost,
                    r.estimated_cost,
                    r.last_seen
                ],
            )
            .unwrap();
        }
    }

    /// 造一个"只有 sessions 表的旧版 Hermes 库"
    fn make_sessions_only_db(path: &Path, rows: &[( &str, &str, &str, f64, i64, i64, i64, i64, i64)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY, model TEXT, billing_provider TEXT,
                 started_at REAL NOT NULL, ended_at REAL,
                 input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
                 cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
                 reasoning_tokens INTEGER DEFAULT 0,
                 estimated_cost_usd REAL, actual_cost_usd REAL);",
        )
        .unwrap();
        for r in rows {
            conn.execute(
                "INSERT INTO sessions
                     (id, model, billing_provider, started_at, input_tokens, output_tokens,
                      cache_read_tokens, cache_write_tokens, reasoning_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8],
            )
            .unwrap();
        }
    }

    fn open_test_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.db")).unwrap();
        db::migrate(&conn).unwrap();
        (dir, conn)
    }

    fn count_hermes(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM usage_record WHERE source='hermes'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn sample() -> Smu<'static> {
        Smu {
            session: "sess-1",
            model: "anthropic/claude-sonnet-4.6",
            provider: "anthropic",
            last_seen: 1_789_196_249.0,
            input: 1000,
            output: 200,
            cache_read: 300,
            cache_write: 50,
            reasoning: 40,
            actual_cost: 0.0,
            estimated_cost: 0.0,
        }
    }

    #[test]
    fn first_scan_backfills_and_maps_buckets() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        make_smu_db(&hermes, &[sample()]);

        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);
        assert_eq!(count_hermes(&conn), 1);

        let (prompt, comp, cached, total, model, provider, session, project, at, rid, note): (
            i64, i64, i64, i64, Option<String>, Option<String>, Option<String>,
            Option<String>, String, Option<String>, Option<String>,
        ) = conn
            .query_row(
                "SELECT prompt_tokens, completion_tokens, cached_tokens, total_tokens,
                        model_name, provider_code, session_id, project, recorded_at, request_id, note
                   FROM usage_record WHERE source='hermes'",
                [],
                |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                        r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?,
                    ))
                },
            )
            .unwrap();
        // 五桶并列相加: prompt = input + cache_read + cache_write
        assert_eq!(prompt, 1350, "prompt 应含缓存读写");
        // reasoning 独立于 output, 一起按输出计价
        assert_eq!(comp, 240, "completion 应含推理 token");
        assert_eq!(cached, 300);
        assert_eq!(total, 1590);
        assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4.6"));
        assert_eq!(provider.as_deref(), Some("anthropic"));
        assert_eq!(session.as_deref(), Some("sess-1"));
        assert_eq!(project.as_deref(), Some("C:\\work\\proj"));
        assert_eq!(
            at,
            chrono::DateTime::from_timestamp_millis(1_789_196_249_000)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        assert!(rid.unwrap().starts_with("hermes|"));
        let note = note.unwrap();
        assert!(note.contains("历史补录"), "{note}");
        assert!(note.contains("推理 40"), "{note}");

        // 再扫一次: 无变化 → 不新增
        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 0);
        assert_eq!(count_hermes(&conn), 1);
    }

    #[test]
    fn second_scan_inserts_only_the_delta() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        make_smu_db(&hermes, &[sample()]);
        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);

        // Hermes 又跑了: input +500, output +10, reasoning +5
        let mut r = sample();
        r.input += 500;
        r.output += 10;
        r.reasoning += 5;
        r.last_seen += 30.0;
        make_smu_db(&hermes, &[r]); // 重建库(单行 REPLACE)
        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);

        let (cnt, p, c): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0)
                   FROM usage_record WHERE source='hermes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cnt, 2);
        assert_eq!(p, 1350 + 500, "只应入库差额");
        assert_eq!(c, 240 + 15);

        // 最新一条应是"增量"且时间戳跟着 last_seen 更新
        let note: String = conn
            .query_row(
                "SELECT note FROM usage_record WHERE source='hermes' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(note.contains("增量"), "{note}");
    }

    #[test]
    fn cursor_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("tracker.db");
        let hermes = dir.path().join("state.db");
        make_smu_db(&hermes, &[sample()]);

        {
            let conn = db::open(&db_path).unwrap();
            db::migrate(&conn).unwrap();
            assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);
        }
        // 模拟应用重启: 新连接、新的内存状态, 但水位线在库里
        {
            let conn = db::open(&db_path).unwrap();
            db::migrate(&conn).unwrap();
            assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 0, "重启后不应重复计数");
            assert_eq!(count_hermes(&conn), 1);
        }
    }

    #[test]
    fn counter_rollback_does_not_write_negative() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        make_smu_db(&hermes, &[sample()]);
        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);

        // Hermes 侧会话被重置/回退
        let mut r = sample();
        r.input = 10;
        make_smu_db(&hermes, &[r]);
        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 0, "回退不应产生记录");
        let p: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(prompt_tokens),0) FROM usage_record WHERE source='hermes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p, 1350);

        // 水位线已回落到 10, 之后的新增量继续正常计数
        let mut r2 = sample();
        r2.input = 110;
        r2.output = 0;
        r2.cache_read = 0;
        r2.cache_write = 0;
        r2.reasoning = 0;
        make_smu_db(&hermes, &[r2]);
        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 1);
        let p: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(prompt_tokens),0) FROM usage_record WHERE source='hermes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p, 1350 + 100);
    }

    #[test]
    fn falls_back_to_sessions_totals_without_smu_table() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        // 旧版 Hermes: 无 session_model_usage
        make_sessions_only_db(
            &hermes,
            &[("sess-old", "gpt-5.4", "openai", 1_789_196_249.0, 800, 100, 200, 30, 20)],
        );

        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);
        let (p, c, cached, model): (i64, i64, i64, Option<String>) = conn
            .query_row(
                "SELECT prompt_tokens, completion_tokens, cached_tokens, model_name
                   FROM usage_record WHERE source='hermes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(p, 800 + 200 + 30);
        assert_eq!(c, 100 + 20);
        assert_eq!(cached, 200);
        assert_eq!(model.as_deref(), Some("gpt-5.4"));
        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 0);
    }

    #[test]
    fn smu_rows_win_over_session_totals() {
        // smu 与 sessions 同时有量: 只信 smu, 不能把同一份用量记两遍
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        make_smu_db(&hermes, &[sample()]);

        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 1, "只入库 smu 那一条");
        assert_eq!(count_hermes(&conn), 1);
    }

    #[test]
    fn profiles_are_scanned_independently() {
        let (dir, conn) = open_test_db();
        let home = dir.path().join("hermes");
        std::fs::create_dir_all(home.join("profiles").join("coder")).unwrap();
        std::fs::create_dir_all(home.join("profiles").join("writer")).unwrap();
        make_smu_db(&home.join("state.db"), &[sample()]);
        let mut a = sample();
        a.session = "sess-a";
        make_smu_db(&home.join("profiles").join("coder").join("state.db"), &[a]);
        let mut b = sample();
        b.session = "sess-b";
        make_smu_db(&home.join("profiles").join("writer").join("state.db"), &[b]);

        let dbs = resolve_dbs(&home);
        assert_eq!(dbs.len(), 3);
        assert_eq!(scan_once(&conn, &dbs).unwrap(), 3);
        assert_eq!(count_hermes(&conn), 3);
        assert_eq!(scan_once(&conn, &dbs).unwrap(), 0);
    }

    #[test]
    fn priced_model_ignores_hermes_reported_cost() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        let mut r = sample();
        r.model = "claude-sonnet-5"; // 内置单价库有
        r.estimated_cost = 9.99;
        make_smu_db(&hermes, &[r]);

        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 1);
        let (cost, src): (Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT cost_usd, cost_source FROM usage_record WHERE source='hermes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(src.as_deref(), Some("computed"), "应按本应用单价估算");
        assert!(cost.is_some() && (cost.unwrap() - 9.99).abs() > 1e-9);
    }

    #[test]
    fn unpriced_model_falls_back_to_hermes_cost() {
        let (dir, conn) = open_test_db();
        let hermes = dir.path().join("state.db");
        let mut r = sample();
        r.model = "some-unpriced-model-xyz";
        r.estimated_cost = 0.75;
        r.actual_cost = 0.0;
        make_smu_db(&hermes, &[r.clone()]);

        assert_eq!(scan_once(&conn, &[hermes.clone()]).unwrap(), 1);
        let (cost, src): (Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT cost_usd, cost_source FROM usage_record WHERE source='hermes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cost, Some(0.75));
        assert_eq!(src.as_deref(), Some("computed"), "estimated 费用标 computed");

        // actual_cost 有值时标 official, 且只记增量
        let mut r2 = r;
        r2.actual_cost = 1.25;
        r2.estimated_cost = 0.0;
        r2.input += 10;
        make_smu_db(&hermes, &[r2]);
        assert_eq!(scan_once(&conn, &[hermes]).unwrap(), 1);
        let (cost, src): (Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT cost_usd, cost_source FROM usage_record WHERE source='hermes'
                  ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cost, Some(1.25 - 0.75), "只记费用差额");
        assert_eq!(src.as_deref(), Some("official"));
    }

    #[test]
    fn resolve_dbs_skips_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        // home 存在但没有 state.db
        let home = dir.path().join("hermes");
        std::fs::create_dir_all(home.join("profiles").join("empty")).unwrap();
        assert!(resolve_dbs(&home).is_empty());
    }

    #[test]
    fn normalise_home_accepts_db_file_portable_root_and_plain_home() {
        let dir = tempfile::tempdir().unwrap();

        // 免安装版根目录 → data\hermes-home
        let root = dir.path().join("Hermes Portable");
        let portable_home = root.join("data").join("hermes-home");
        std::fs::create_dir_all(&portable_home).unwrap();
        assert_eq!(normalise_home(&root.display().to_string()), portable_home);
        assert_eq!(
            normalise_home(&portable_home.display().to_string()),
            portable_home,
            "已经是 home 时原样返回"
        );

        // 直接指向 state.db → 取其所在目录
        let db = portable_home.join("state.db");
        std::fs::write(&db, b"x").unwrap();
        assert_eq!(normalise_home(&db.display().to_string()), portable_home);

        // 普通 home(无 data\hermes-home)→ 原样
        let plain = dir.path().join("plain-home");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(normalise_home(&plain.display().to_string()), plain);
    }

    /// 写入设置(seed_settings 已插入 hermes_home='', 这里覆盖)
    fn set_hermes_home(conn: &Connection, value: &str) {
        conn.execute(
            "INSERT INTO kv_settings(key, value) VALUES('hermes_home', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![value],
        )
        .unwrap();
    }

    #[test]
    fn setting_overrides_default_home_and_scans() {
        let (dir, conn) = open_test_db();
        let home = dir.path().join("hermes-home");
        std::fs::create_dir_all(&home).unwrap();
        make_smu_db(&home.join("state.db"), &[sample()]);

        set_hermes_home(&conn, &home.display().to_string());

        let cfg = load_config(&conn);
        assert_eq!(cfg.homes, vec![home.clone()]);
        assert_eq!(cfg.dbs, vec![home.join("state.db")]);
        assert!(cfg.error.is_none(), "{:?}", cfg.error);

        assert_eq!(scan_once(&conn, &cfg.dbs).unwrap(), 1);
        assert_eq!(count_hermes(&conn), 1);
    }

    #[test]
    fn setting_pointing_at_portable_root_is_normalised() {
        let (dir, conn) = open_test_db();
        let root = dir.path().join("Hermes Agent CN Desktop Portable");
        let home = root.join("data").join("hermes-home");
        std::fs::create_dir_all(&home).unwrap();
        make_smu_db(&home.join("state.db"), &[sample()]);

        set_hermes_home(&conn, &root.display().to_string());

        let cfg = load_config(&conn);
        assert_eq!(cfg.homes, vec![home.clone()]);
        assert_eq!(cfg.dbs.len(), 1);
        assert!(cfg.error.is_none());
    }

    #[test]
    fn home_from_exe_walks_up_to_portable_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Hermes Portable");
        let home = root.join("data").join("hermes-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("state.db"), b"x").unwrap();

        // 桌面主程序: <root>\Hermes Agent.exe
        let desktop = root.join("Hermes Agent CN Desktop.exe");
        std::fs::write(&desktop, b"x").unwrap();
        assert_eq!(home_from_exe(&desktop), Some(home.clone()));

        // 运行时子进程: <root>\data\versions\0.19.0\hermes-agent-runtime.exe (上溯 3 级)
        let runtime_dir = root.join("data").join("versions").join("0.19.0-cn.7");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let runtime = runtime_dir.join("hermes-agent-cn-runtime.exe");
        std::fs::write(&runtime, b"x").unwrap();
        assert_eq!(home_from_exe(&runtime), Some(home.clone()));

        // 无关进程/无 state.db → None
        let other = dir.path().join("NotHermes.exe");
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(home_from_exe(&other), None);
    }

    #[test]
    fn discovered_portable_home_is_merged_with_default() {
        let (dir, conn) = open_test_db();
        let root = dir.path().join("Hermes Portable");
        let home = root.join("data").join("hermes-home");
        std::fs::create_dir_all(&home).unwrap();
        make_smu_db(&home.join("state.db"), &[sample()]);

        // 未显式设置 + 无 HERMES_HOME: 走"平台默认 + 进程识别"合并
        assert!(std::env::var("HERMES_HOME").is_err(), "测试环境不应设置 HERMES_HOME");
        set_hermes_home(&conn, "");
        let cfg = build_config(&conn, vec![home.clone()]);
        assert_eq!(cfg.dbs, vec![home.join("state.db")], "应识别出免安装目录");
        assert!(cfg.homes.contains(&home));

        assert_eq!(scan_once(&conn, &cfg.dbs).unwrap(), 1);
        assert_eq!(count_hermes(&conn), 1);
        // 自动识别到的目录也会显示在前端状态里
        let info = info_from(&cfg, true);
        assert!(info.state_db.ends_with("state.db"));
        assert!(info.hermes_home.contains("Hermes Portable"));
    }

    #[test]
    fn discovered_home_is_ignored_when_setting_is_explicit() {
        let (dir, conn) = open_test_db();
        let explicit = dir.path().join("explicit-home");
        std::fs::create_dir_all(&explicit).unwrap();
        make_smu_db(&explicit.join("state.db"), &[sample()]);
        let other = dir.path().join("other-home");
        std::fs::create_dir_all(other.join("data").join("hermes-home")).unwrap();
        make_smu_db(&other.join("data").join("hermes-home").join("state.db"), &[sample()]);

        set_hermes_home(&conn, &explicit.display().to_string());
        let cfg = build_config(&conn, vec![other.join("data").join("hermes-home")]);
        assert_eq!(cfg.homes, vec![explicit.clone()], "显式设置优先, 不再猜");
        assert_eq!(cfg.dbs, vec![explicit.join("state.db")]);
    }

    #[test]
    fn missing_db_reports_hint_but_thread_stays_up() {
        let (dir, conn) = open_test_db();
        let home = dir.path().join("empty-home");
        std::fs::create_dir_all(&home).unwrap();
        set_hermes_home(&conn, &home.display().to_string());

        let cfg = load_config(&conn);
        assert!(cfg.dbs.is_empty());
        let info = info_from(&cfg, true);
        assert!(info.started, "线程在跑");
        assert!(info.error.unwrap().contains("state.db"), "应给出可操作的提示");
        assert!(info.state_db.ends_with("state.db"), "仍展示预期路径");
    }

    /// 端到端(手动触发): 扫描真实的 Hermes home 到临时库, 不触碰应用库。
    /// 运行: cargo test --lib scan_real_hermes_home -- --ignored --nocapture
    /// 自定义目录: 先设 HERMES_HOME(或在下面 set_hermes_home)。
    #[test]
    #[ignore]
    fn scan_real_hermes_home() {
        let (_dir, conn) = open_test_db();
        let cfg = load_config(&conn);
        if cfg.dbs.is_empty() {
            eprintln!(
                "[hermes-e2e] skip: {} 下没有 state.db",
                cfg.homes
                    .iter()
                    .map(|h| h.display().to_string())
                    .collect::<Vec<_>>()
                    .join("、")
            );
            return;
        }
        eprintln!(
            "[hermes-e2e] home={} 库={}",
            cfg.homes
                .iter()
                .map(|h| h.display().to_string())
                .collect::<Vec<_>>()
                .join("、"),
            cfg.dbs.len()
        );
        let n = scan_once(&conn, &cfg.dbs).unwrap();
        let (calls, p, c, ca, cost): (i64, i64, i64, i64, f64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost_usd),0)
                   FROM usage_record WHERE source='hermes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        eprintln!("[hermes-e2e] 首次扫描新增={n}");
        eprintln!("[hermes-e2e] 记录={calls} 输入={p} 输出={c} 缓存={ca} 费用=${cost:.4}");
        assert_eq!(scan_once(&conn, &cfg.dbs).unwrap(), 0, "第二轮不应新增");
    }

    /// 进程识别(手动触发): 打印从"正在运行的 Hermes 进程"反推出来的数据目录。
    /// 运行: cargo test --lib discover_running_homes_real -- --ignored --nocapture
    #[test]
    #[ignore]
    fn discover_running_homes_real() {
        let homes = discover_running_homes();
        eprintln!("[hermes-discover] 进程路径数={}", process_image_paths().len());
        eprintln!("[hermes-discover] 识别到 {} 个 home: {:#?}", homes.len(), homes);
        for h in &homes {
            eprintln!("[hermes-discover] {} -> dbs={:?}", h.display(), resolve_dbs(h));
        }
    }
}
