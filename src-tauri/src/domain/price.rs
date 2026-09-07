//! 模型单价: 单位统一为 USD / 1M tokens(厂商官方结算币种)。
//! 展示层按设置中的汇率换算为 CNY。价格数据为 2026 年来源快照,
//! 全部以 builtin 行入库, 用户可在设置页以 custom 行覆盖。

use rusqlite::{params, Connection, Result};

/// 内置价格快照条目
pub struct BuiltinPrice {
    pub provider_code: &'static str,
    pub provider_name: &'static str,
    pub model_name: &'static str,
    /// 每 1M 输入(未命中缓存) tokens 的 USD 价
    pub input_per_mtok: f64,
    /// 每 1M 输出 tokens 的 USD 价
    pub output_per_mtok: f64,
    /// 每 1M 缓存命中输入 tokens 的 USD 价(None 表示无缓存计费)
    pub cached_input_per_mtok: Option<f64>,
}

/// 2026-09 快照(DeepSeek 官方定价页 / BenchLM OpenAI、Anthropic 汇总)。
/// DeepSeek 价格含 peak/off-peak 与 cache hit/miss, 此处取 peak 档 + cache-hit 单独列。
/// 注: 官方可能调价, 请以官方页面为准, 本表可被 custom 覆盖。
pub const BUILTIN: &[BuiltinPrice] = &[
    // ---------------- DeepSeek ----------------
    BuiltinPrice {
        provider_code: "deepseek",
        provider_name: "DeepSeek",
        model_name: "deepseek-v4-flash",
        input_per_mtok: 0.44,      // peak cache-miss input
        output_per_mtok: 1.32,     // peak output
        cached_input_per_mtok: Some(0.014), // peak cache-hit input
    },
    BuiltinPrice {
        provider_code: "deepseek",
        provider_name: "DeepSeek",
        model_name: "deepseek-v4-pro",
        input_per_mtok: 1.32,
        output_per_mtok: 3.96,
        cached_input_per_mtok: Some(0.044),
    },
    BuiltinPrice {
        provider_code: "deepseek",
        provider_name: "DeepSeek",
        model_name: "deepseek-v4-flash-vision-exp",
        input_per_mtok: 0.44,
        output_per_mtok: 1.32,
        cached_input_per_mtok: Some(0.014),
    },
    // ---------------- OpenAI ----------------
    BuiltinPrice {
        provider_code: "openai",
        provider_name: "OpenAI",
        model_name: "gpt-5.6-sol",
        input_per_mtok: 5.0,
        output_per_mtok: 30.0,
        cached_input_per_mtok: Some(0.5),
    },
    BuiltinPrice {
        provider_code: "openai",
        provider_name: "OpenAI",
        model_name: "gpt-5.6-terra",
        input_per_mtok: 2.5,
        output_per_mtok: 15.0,
        cached_input_per_mtok: Some(0.25),
    },
    BuiltinPrice {
        provider_code: "openai",
        provider_name: "OpenAI",
        model_name: "gpt-5.6-luna",
        input_per_mtok: 1.0,
        output_per_mtok: 6.0,
        cached_input_per_mtok: Some(0.1),
    },
    BuiltinPrice {
        provider_code: "openai",
        provider_name: "OpenAI",
        model_name: "gpt-5.5",
        input_per_mtok: 5.0,
        output_per_mtok: 30.0,
        cached_input_per_mtok: Some(0.5),
    },
    BuiltinPrice {
        provider_code: "openai",
        provider_name: "OpenAI",
        model_name: "gpt-5.5-pro",
        input_per_mtok: 30.0,
        output_per_mtok: 180.0,
        cached_input_per_mtok: None,
    },
    // ---------------- Anthropic ----------------
    BuiltinPrice {
        provider_code: "anthropic",
        provider_name: "Anthropic",
        model_name: "claude-opus-5",
        input_per_mtok: 5.0,
        output_per_mtok: 25.0,
        cached_input_per_mtok: Some(0.5),
    },
    BuiltinPrice {
        provider_code: "anthropic",
        provider_name: "Anthropic",
        model_name: "claude-sonnet-5",
        input_per_mtok: 2.0,
        output_per_mtok: 10.0,
        cached_input_per_mtok: Some(0.2),
    },
    BuiltinPrice {
        provider_code: "anthropic",
        provider_name: "Anthropic",
        model_name: "claude-haiku-4-5",
        input_per_mtok: 1.0,
        output_per_mtok: 5.0,
        cached_input_per_mtok: Some(0.1),
    },
];

/// 幂等写入内置价格。仅当某 provider 不存在时插入该 provider 的全部模型,
/// 已存在 custom 覆盖行时不会回写(保证用户改动不被覆盖)。
pub fn seed_builtin(conn: &Connection) -> Result<()> {
    for item in BUILTIN {
        // provider
        conn.execute(
            "INSERT OR IGNORE INTO provider(code, name) VALUES(?1, ?2)",
            params![item.provider_code, item.provider_name],
        )?;
        let provider_id: i64 = conn.query_row(
            "SELECT id FROM provider WHERE code = ?1",
            params![item.provider_code],
            |r| r.get(0),
        )?;
        // model
        conn.execute(
            "INSERT OR IGNORE INTO model(provider_id, name, builtin) VALUES(?1, ?2, 1)",
            params![provider_id, item.model_name],
        )?;
        let model_id: i64 = conn.query_row(
            "SELECT id FROM model WHERE provider_id = ?1 AND name = ?2",
            params![provider_id, item.model_name],
            |r| r.get(0),
        )?;
        // price (builtin 行仅在不存在时插入)
        conn.execute(
            "INSERT OR IGNORE INTO price(model_id, currency, input_per_mtok, output_per_mtok, cached_input_per_mtok, source)
             VALUES(?1, 'USD', ?2, ?3, ?4, 'builtin')",
            params![
                model_id,
                item.input_per_mtok,
                item.output_per_mtok,
                item.cached_input_per_mtok,
            ],
        )?;
    }
    Ok(())
}

/// 查询某模型的生效单价(custom 优先, 其次 builtin)。
#[derive(Debug, Clone)]
pub struct PriceRow {
    pub model_id: i64,
    pub model_name: String,
    pub provider_code: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cached_input_per_mtok: Option<f64>,
    pub source: String,
}

/// 按模型名(全库唯一假定, 跨厂商名称冲突少见)查生效价。
pub fn price_for_model(conn: &Connection, model_name: &str) -> Result<Option<PriceRow>> {
    let mut stmt = conn.prepare(
        "SELECT p.model_id, m.name, pr.code, p.input_per_mtok, p.output_per_mtok,
                p.cached_input_per_mtok, p.source
         FROM price p
         JOIN model m ON m.id = p.model_id
         JOIN provider pr ON pr.id = m.provider_id
         WHERE m.name = ?1 AND p.currency = 'USD'
         ORDER BY CASE p.source WHEN 'custom' THEN 0 ELSE 1 END
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![model_name])?;
    let row = rows.next()?;
    match row {
        Some(r) => Ok(Some(PriceRow {
            model_id: r.get(0)?,
            model_name: r.get(1)?,
            provider_code: r.get(2)?,
            input_per_mtok: r.get(3)?,
            output_per_mtok: r.get(4)?,
            cached_input_per_mtok: r.get(5)?,
            source: r.get(6)?,
        })),
        None => Ok(None),
    }
}

/// 依据单价与 token 数估算 USD 成本。
/// 若 prompt_tokens 已包含缓存命中部分, 传入 cached_tokens 以便分别计价。
pub fn estimate_cost_usd(
    price: &PriceRow,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cached_tokens: Option<i64>,
) -> Option<f64> {
    let prompt = prompt_tokens.unwrap_or(0) as f64;
    let completion = completion_tokens.unwrap_or(0) as f64;
    if prompt <= 0.0 && completion <= 0.0 {
        return None;
    }
    let cached = (cached_tokens.unwrap_or(0) as f64).clamp(0.0, prompt);
    let uncached = prompt - cached;

    let cached_price = price
        .cached_input_per_mtok
        .unwrap_or(price.input_per_mtok);

    let cost = (uncached * price.input_per_mtok + cached * cached_price
        + completion * price.output_per_mtok)
        / 1_000_000.0;
    Some(cost)
}

/// 列出全部模型及其生效单价(供设置页展示)。
#[derive(serde::Serialize)]
pub struct ModelPriceView {
    pub model_id: i64,
    pub provider_code: String,
    pub provider_name: String,
    pub model_name: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cached_input_per_mtok: Option<f64>,
    pub source: String,
}

pub fn list_all(conn: &Connection) -> Result<Vec<ModelPriceView>> {
    let mut stmt = conn.prepare(
        "SELECT p.model_id, pr.code, pr.name, m.name,
                p.input_per_mtok, p.output_per_mtok, p.cached_input_per_mtok, p.source
         FROM price p
         JOIN model m ON m.id = p.model_id
         JOIN provider pr ON pr.id = m.provider_id
         WHERE p.currency = 'USD'
           AND p.source = CASE
                 WHEN EXISTS (SELECT 1 FROM price c
                              WHERE c.model_id = p.model_id AND c.currency='USD' AND c.source='custom')
                 THEN 'custom' ELSE 'builtin' END
         ORDER BY pr.name, m.name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ModelPriceView {
            model_id: r.get(0)?,
            provider_code: r.get(1)?,
            provider_name: r.get(2)?,
            model_name: r.get(3)?,
            input_per_mtok: r.get(4)?,
            output_per_mtok: r.get(5)?,
            cached_input_per_mtok: r.get(6)?,
            source: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// upsert 自定义单价: 模型不存在则顺带创建 custom model。
/// 返回受影响的模型 id。
pub fn upsert_custom(
    conn: &Connection,
    provider_code: &str,
    provider_name: &str,
    model_name: &str,
    input_per_mtok: f64,
    output_per_mtok: f64,
    cached_input_per_mtok: Option<f64>,
) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO provider(code, name) VALUES(?1, ?2)",
        params![provider_code, provider_name],
    )?;
    let provider_id: i64 = conn.query_row(
        "SELECT id FROM provider WHERE code = ?1",
        params![provider_code],
        |r| r.get(0),
    )?;
    // builtin 同名模型存在则复用, 否则新建 model
    conn.execute(
        "INSERT OR IGNORE INTO model(provider_id, name, builtin) VALUES(?1, ?2, 0)",
        params![provider_id, model_name],
    )?;
    let model_id: i64 = conn.query_row(
        "SELECT id FROM model WHERE provider_id = ?1 AND name = ?2",
        params![provider_id, model_name],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO price(model_id, currency, input_per_mtok, output_per_mtok, cached_input_per_mtok, source, updated_at)
         VALUES(?1, 'USD', ?2, ?3, ?4, 'custom', datetime('now'))
         ON CONFLICT(model_id, currency, source) DO UPDATE SET
            input_per_mtok = excluded.input_per_mtok,
            output_per_mtok = excluded.output_per_mtok,
            cached_input_per_mtok = excluded.cached_input_per_mtok,
            updated_at = datetime('now')",
        params![
            model_id,
            input_per_mtok,
            output_per_mtok,
            cached_input_per_mtok,
        ],
    )?;
    Ok(model_id)
}
