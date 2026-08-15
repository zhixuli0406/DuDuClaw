//! Code Mode **Phase 0 measurement gate** (WP-H2 / WP-6E).
//!
//! Design: `commercial/docs/DESIGN-code-mode-2026-08.md` §8 / §8.1.
//!
//! ## Why this module exists
//!
//! Before spending L-sized engineering on an in-process JS engine, the design
//! demands three *real* numbers off the paths that would actually benefit
//! (§2: `openai_compat` / direct-API / local inference — never the CLI
//! runtimes):
//!
//! 1. **schema share** — how much of a request is nothing but tool
//!    descriptions (the "progressive disclosure" saving, which needs no
//!    engine at all);
//! 2. **rounds** — provider calls per turn (the "merge multi-step work"
//!    saving, which does need one);
//! 3. **cache hit rate** — because §7.1 already found the Claude path's
//!    O(k²) re-send cost is almost entirely absorbed by prompt cache. If the
//!    compat path's *implicit* prefix cache does the same, the whole benefit
//!    premise collapses and Code Mode should be rejected.
//!
//! ## Zero behavior change
//!
//! Everything here is observation. [`ToolLoopProbe`] is a [`ChatProvider`]
//! decorator layered *on top of* each path's existing provider (the
//! `UsageTap` / `RecordingProvider` decorators keep doing the billing) —
//! it forwards `complete`/`stream` verbatim and only counts. Every write is
//! fail-open: a probe error is logged at debug and the turn proceeds.
//!
//! ## Why a new table and not a new file
//!
//! Round count is the one number that *cannot* be derived from existing
//! telemetry: `token_usage` records one summed row per turn, and
//! `tool_calls.jsonl` is gated by `is_state_changing` so read-only tools
//! leave no trace (design §1.3 gap 3). So one collection point is
//! unavoidable — it lives as an additive table inside the existing
//! `cost_telemetry.db`, using the same short-lived-WAL-connection pattern as
//! `cost_telemetry::record_ephemeral_parent_at`. No new store, no new file,
//! no schema change to `token_usage`.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rusqlite::{params, Connection};

use duduclaw_llm::{ChatProvider, ChatRequest, ChatResponse, LlmError, StreamEvent};

// ---------------------------------------------------------------------------
// Gate thresholds — design §8.1. Change a threshold HERE and nowhere else.
// ---------------------------------------------------------------------------

/// **G0** — sample floor. Below this the report refuses to conclude.
/// Self-imposed (design §8.1): the design's own §7.1 figures are `n=2` and
/// self-labelled "anecdote, not statistics"; a gate without a floor would
/// repeat that mistake.
pub const GATE_MIN_TURNS: u64 = 30;

/// **G1** — tool-schema tokens carried by a single provider call.
/// Above this, progressive disclosure (saving source ① — no engine needed)
/// is worth doing. Design §8 "門檻建議：單回合 schema tokens > 8k".
pub const GATE_SCHEMA_TOKENS: u64 = 8_000;

/// **G2** — mean provider calls per turn. Above this, merging multi-step work
/// into one model call (saving source ② — needs the engine) is worth doing.
/// Design §8 "平均 provider 呼叫 > 4".
pub const GATE_AVG_PROVIDER_CALLS: f64 = 4.0;

/// **G3** (veto) — cache hit rate at or above this means the provider's
/// implicit prefix cache already absorbs the re-sent prefix, exactly as
/// §7.1 found for the Claude path, so the benefit premise is gone.
/// Threshold reuses the project's existing cache yardstick (the cache-aware
/// compression guard skips compression above 50% efficiency) rather than
/// inventing a second dialect.
pub const GATE_CACHE_ABSORBED: f64 = 0.50;

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

/// Which of the three §2 beneficiary paths produced an observation. Stored as
/// a string column so a future path needs no migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbePath {
    /// `runtime/openai_compat.rs` — API-mode Grok / DeepSeek / MiniMax…
    OpenAiCompat,
    /// `claude_runner.rs` direct-API tool loop.
    DirectApi,
    /// `local_llm.rs` local-inference tool loop.
    LocalInference,
}

impl ProbePath {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbePath::OpenAiCompat => "openai_compat",
            ProbePath::DirectApi => "direct_api",
            ProbePath::LocalInference => "local_inference",
        }
    }
}

/// One turn's worth of measurement, as read off a finished [`ToolLoopProbe`].
#[derive(Debug, Clone, PartialEq)]
pub struct ToolLoopObservation {
    pub agent_id: String,
    pub path: ProbePath,
    pub model: String,
    /// Provider round-trips this turn (1 = the model answered without tools).
    pub provider_calls: u64,
    /// Tool definitions advertised (post capability filter).
    pub tool_defs: u64,
    /// Estimated tokens spent on tool schemas in ONE provider call.
    /// Heuristic (`ChatRequest::estimate_tool_schema_tokens`), not a
    /// provider tokenizer — same estimator class as the design's §7.1 figures.
    pub schema_tokens: u64,
    /// Estimated total input tokens of the FIRST provider call — the honest
    /// denominator for "what fraction of a request is schema" (later rounds
    /// grow with tool results, which would flatter the ratio).
    pub first_call_input_tokens: u64,
    /// Sum of provider-reported non-cached prompt tokens across all rounds.
    pub billed_input_tokens: u64,
    /// Sum of provider-reported cache-read tokens across all rounds.
    pub cache_read_tokens: u64,
    /// Rounds whose usage carried a cache field at all. When this is 0 the
    /// provider is silent about caching and G3's verdict is not trustworthy —
    /// reported explicitly rather than folded into a 0% hit rate.
    pub cache_reporting_calls: u64,
}

// ---------------------------------------------------------------------------
// The decorator
// ---------------------------------------------------------------------------

/// A [`ChatProvider`] decorator that counts what the Phase 0 gate needs.
///
/// Wraps a borrowed provider so it can be layered over the existing per-path
/// decorators without taking ownership. Pure passthrough: the inner
/// provider's response is returned verbatim and errors propagate unchanged.
pub struct ToolLoopProbe<'a> {
    inner: &'a dyn ChatProvider,
    provider_calls: AtomicU64,
    tool_defs: AtomicU64,
    schema_tokens: AtomicU64,
    first_call_input_tokens: AtomicU64,
    billed_input_tokens: AtomicU64,
    cache_read_tokens: AtomicU64,
    cache_reporting_calls: AtomicU64,
}

impl<'a> ToolLoopProbe<'a> {
    pub fn new(inner: &'a dyn ChatProvider) -> Self {
        Self {
            inner,
            provider_calls: AtomicU64::new(0),
            tool_defs: AtomicU64::new(0),
            schema_tokens: AtomicU64::new(0),
            first_call_input_tokens: AtomicU64::new(0),
            billed_input_tokens: AtomicU64::new(0),
            cache_read_tokens: AtomicU64::new(0),
            cache_reporting_calls: AtomicU64::new(0),
        }
    }

    /// Measure the request side. Schema size is taken as the MAX across
    /// rounds: the loop re-sends `req.tools` unchanged every round, so max ==
    /// per-call cost, and it stays correct if a future path ever narrows the
    /// tool set mid-loop.
    fn observe_request(&self, req: &ChatRequest) {
        let n = self.provider_calls.fetch_add(1, Ordering::Relaxed);
        let schema = req.estimate_tool_schema_tokens();
        self.tool_defs
            .fetch_max(req.tools.len() as u64, Ordering::Relaxed);
        self.schema_tokens.fetch_max(schema, Ordering::Relaxed);
        if n == 0 {
            self.first_call_input_tokens
                .store(req.estimate_input_tokens(), Ordering::Relaxed);
        }
    }

    fn observe_response(&self, resp: &ChatResponse) {
        self.billed_input_tokens
            .fetch_add(resp.usage.input_tokens, Ordering::Relaxed);
        self.cache_read_tokens
            .fetch_add(resp.usage.cache_read_tokens, Ordering::Relaxed);
        // A provider that never populates a cache field leaves `cache_read` at
        // 0, which is indistinguishable from "cache missed". Count the rounds
        // that actually reported something so the report can say so.
        if resp.usage.cache_read_tokens > 0 || resp.usage.cache_write_tokens > 0 {
            self.cache_reporting_calls.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot the counters into an observation. Safe to call even if the
    /// loop errored — a partial turn is still a truthful observation.
    pub fn finish(&self, agent_id: &str, path: ProbePath, model: &str) -> ToolLoopObservation {
        ToolLoopObservation {
            agent_id: agent_id.to_string(),
            path,
            model: model.to_string(),
            provider_calls: self.provider_calls.load(Ordering::Relaxed),
            tool_defs: self.tool_defs.load(Ordering::Relaxed),
            schema_tokens: self.schema_tokens.load(Ordering::Relaxed),
            first_call_input_tokens: self.first_call_input_tokens.load(Ordering::Relaxed),
            billed_input_tokens: self.billed_input_tokens.load(Ordering::Relaxed),
            cache_read_tokens: self.cache_read_tokens.load(Ordering::Relaxed),
            cache_reporting_calls: self.cache_reporting_calls.load(Ordering::Relaxed),
        }
    }

    /// Snapshot and persist in one step, swallowing every failure (debug log
    /// only). The single call site pattern for the three wired paths.
    pub fn finish_and_record(&self, agent_id: &str, path: ProbePath, model: &str) {
        let obs = self.finish(agent_id, path, model);
        if obs.provider_calls == 0 {
            return; // nothing happened — never write an empty row
        }
        if let Err(e) = record(&duduclaw_core::duduclaw_home(), &obs) {
            tracing::debug!(error = %e, "tool_loop_probe: observation not recorded");
        }
    }
}

#[async_trait]
impl ChatProvider for ToolLoopProbe<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.observe_request(req);
        let resp = self.inner.complete(req).await?;
        self.observe_response(&resp);
        Ok(resp)
    }

    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        // The tool loop is non-streaming; delegate untouched (and do not
        // count, since we never see the finished usage here).
        self.inner.stream(req).await
    }
}

// ---------------------------------------------------------------------------
// Store — additive table inside the existing cost_telemetry.db
// ---------------------------------------------------------------------------

fn db_path(home_dir: &std::path::Path) -> std::path::PathBuf {
    home_dir.join("cost_telemetry.db")
}

fn open(home_dir: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path(home_dir)).map_err(|e| format!("open telemetry db: {e}"))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;
         CREATE TABLE IF NOT EXISTS tool_loop_probe (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             agent_id TEXT NOT NULL,
             path TEXT NOT NULL,
             model TEXT NOT NULL DEFAULT '',
             provider_calls INTEGER NOT NULL DEFAULT 0,
             tool_defs INTEGER NOT NULL DEFAULT 0,
             schema_tokens INTEGER NOT NULL DEFAULT 0,
             first_call_input_tokens INTEGER NOT NULL DEFAULT 0,
             billed_input_tokens INTEGER NOT NULL DEFAULT 0,
             cache_read_tokens INTEGER NOT NULL DEFAULT 0,
             cache_reporting_calls INTEGER NOT NULL DEFAULT 0,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_tool_loop_probe_time
             ON tool_loop_probe(created_at);",
    )
    .map_err(|e| format!("tool_loop_probe schema: {e}"))?;
    Ok(conn)
}

/// Append one observation. Own short-lived WAL connection (same pattern as
/// `cost_telemetry::record_ephemeral_parent_at`) so this works from the
/// gateway and the mcp-server process alike.
pub fn record(home_dir: &std::path::Path, obs: &ToolLoopObservation) -> Result<(), String> {
    let conn = open(home_dir)?;
    conn.execute(
        "INSERT INTO tool_loop_probe (
             agent_id, path, model, provider_calls, tool_defs, schema_tokens,
             first_call_input_tokens, billed_input_tokens, cache_read_tokens,
             cache_reporting_calls, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            obs.agent_id,
            obs.path.as_str(),
            obs.model,
            obs.provider_calls,
            obs.tool_defs,
            obs.schema_tokens,
            obs.first_call_input_tokens,
            obs.billed_input_tokens,
            obs.cache_read_tokens,
            obs.cache_reporting_calls,
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .map_err(|e| format!("insert tool_loop_probe: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Summary + gate evaluation
// ---------------------------------------------------------------------------

/// Aggregated measurements over a window, per §8.1's four inputs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeSummary {
    /// Number of observed turns (G0's input).
    pub turns: u64,
    pub agents: u64,
    /// Mean provider calls per turn (G2's input).
    pub avg_provider_calls: f64,
    pub max_provider_calls: u64,
    /// Mean per-call tool-schema tokens (G1's input).
    pub avg_schema_tokens: u64,
    pub max_schema_tokens: u64,
    pub avg_tool_defs: u64,
    /// Mean `schema_tokens / first_call_input_tokens` — "how much of a
    /// request is just tool descriptions".
    pub schema_share: f64,
    pub billed_input_tokens: u64,
    pub cache_read_tokens: u64,
    /// `cache_read / (billed_input + cache_read)` (G3's input).
    pub cache_hit_rate: f64,
    /// Fraction of turns in which the provider reported any cache field.
    /// Low ⇒ G3's verdict is not trustworthy.
    pub cache_reporting_share: f64,
    /// Per-path turn counts, ordered by count descending.
    pub by_path: Vec<(String, u64)>,
}

/// The §8.1 decision table's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// G0 failed — keep collecting, do not decide.
    InsufficientData,
    /// G3 veto — implicit prefix cache already absorbs the cost.
    RejectCacheAbsorbed,
    /// Neither G1 nor G2 cleared.
    RejectBelowThreshold,
    /// G1 or G2 cleared and G3 did not veto.
    Proceed,
}

impl GateVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            GateVerdict::InsufficientData => "INSUFFICIENT_DATA",
            GateVerdict::RejectCacheAbsorbed => "REJECT_CACHE_ABSORBED",
            GateVerdict::RejectBelowThreshold => "REJECT_BELOW_THRESHOLD",
            GateVerdict::Proceed => "PROCEED",
        }
    }

    /// zh-TW action line — what the operator should actually do next.
    pub fn action_zh(self) -> &'static str {
        match self {
            GateVerdict::InsufficientData => {
                "繼續收集，不拍板（受益路徑用量不足；本機用量為零本身就是設計文件 R1 的實證）"
            }
            GateVerdict::RejectCacheAbsorbed => {
                "停案：隱式前綴快取已吸收重送成本，與 §7.1 的 Claude 路同因；設計文件轉 rejected/"
            }
            GateVerdict::RejectBelowThreshold => {
                "停案：schema 與 round-trip 成本皆未達門檻；設計文件轉 rejected/"
            }
            GateVerdict::Proceed => "放行 Phase 0-b（max_tool_iters config 化／穩定前綴／漸進揭露）",
        }
    }
}

/// One evaluated criterion, for a checkable report line.
#[derive(Debug, Clone, PartialEq)]
pub struct GateCheck {
    pub id: &'static str,
    pub label: &'static str,
    /// Rendered "actual vs threshold".
    pub detail: String,
    pub passed: bool,
}

/// The full gate report: every criterion plus the verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct GateReport {
    pub checks: Vec<GateCheck>,
    pub verdict: GateVerdict,
    /// Set when the cache signal is too sparse for G3 to be trusted.
    pub cache_signal_warning: Option<String>,
}

/// Evaluate §8.1's decision table. Pure function of the summary — unit-tested
/// without touching a database.
pub fn evaluate_gate(s: &ProbeSummary) -> GateReport {
    let g0 = s.turns >= GATE_MIN_TURNS;
    let g1 = s.avg_schema_tokens > GATE_SCHEMA_TOKENS;
    let g2 = s.avg_provider_calls > GATE_AVG_PROVIDER_CALLS;
    let g3 = s.cache_hit_rate >= GATE_CACHE_ABSORBED;

    let checks = vec![
        GateCheck {
            id: "G0",
            label: "樣本量足夠",
            detail: format!("{} turns（門檻 ≥ {GATE_MIN_TURNS}）", s.turns),
            passed: g0,
        },
        GateCheck {
            id: "G1",
            label: "schema tokens 夠大",
            detail: format!(
                "平均 {} tokens／次呼叫，佔首呼叫輸入 {:.1}%（門檻 > {GATE_SCHEMA_TOKENS}）",
                s.avg_schema_tokens,
                s.schema_share * 100.0
            ),
            passed: g1,
        },
        GateCheck {
            id: "G2",
            label: "provider 呼叫夠多",
            detail: format!(
                "平均 {:.2} 次／輪，最高 {}（門檻 > {GATE_AVG_PROVIDER_CALLS:.1}）",
                s.avg_provider_calls, s.max_provider_calls
            ),
            passed: g2,
        },
        GateCheck {
            // Veto criterion: `passed` means "the veto did NOT fire".
            id: "G3",
            label: "cache 未吸收成本（否決條件）",
            detail: format!(
                "命中率 {:.1}%（≥ {:.0}% 即否決）",
                s.cache_hit_rate * 100.0,
                GATE_CACHE_ABSORBED * 100.0
            ),
            passed: !g3,
        },
    ];

    let verdict = if !g0 {
        GateVerdict::InsufficientData
    } else if g3 {
        GateVerdict::RejectCacheAbsorbed
    } else if g1 || g2 {
        GateVerdict::Proceed
    } else {
        GateVerdict::RejectBelowThreshold
    };

    // Honesty guard: a provider that never reports a cache field yields
    // hit rate 0, which silently biases G3 toward "do not veto" (i.e. toward
    // building Code Mode). Say so out loud instead.
    let cache_signal_warning = if g0 && s.cache_reporting_share < 0.10 {
        Some(format!(
            "只有 {:.0}% 的輪次收到 provider 回報的 cache 欄位；\
             G3 的 0% 命中率可能是「沒回報」而非「沒命中」，該項結論須人工判讀。",
            s.cache_reporting_share * 100.0
        ))
    } else {
        None
    };

    GateReport { checks, verdict, cache_signal_warning }
}

/// Aggregate the probe table over the last `days`, optionally for one agent.
pub fn summarize(
    home_dir: &std::path::Path,
    days: u64,
    agent: Option<&str>,
) -> Result<ProbeSummary, String> {
    let conn = open(home_dir)?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days.clamp(1, 3650) as i64))
        .to_rfc3339();

    // One pass for the scalar aggregates. `schema_share` is averaged per row
    // (not computed from the two column means) so a single huge turn cannot
    // dominate the ratio.
    let (turns, agents, sum_calls, max_calls, sum_schema, max_schema, sum_defs, sum_share, billed, cache_read, reporting_turns): (
        u64, u64, u64, u64, u64, u64, u64, f64, u64, u64, u64,
    ) = conn
        .query_row(
            "SELECT
                 COUNT(*),
                 COUNT(DISTINCT agent_id),
                 COALESCE(SUM(provider_calls),0),
                 COALESCE(MAX(provider_calls),0),
                 COALESCE(SUM(schema_tokens),0),
                 COALESCE(MAX(schema_tokens),0),
                 COALESCE(SUM(tool_defs),0),
                 COALESCE(SUM(CASE WHEN first_call_input_tokens > 0
                                   THEN CAST(schema_tokens AS REAL) / first_call_input_tokens
                                   ELSE 0 END), 0.0),
                 COALESCE(SUM(billed_input_tokens),0),
                 COALESCE(SUM(cache_read_tokens),0),
                 COALESCE(SUM(CASE WHEN cache_reporting_calls > 0 THEN 1 ELSE 0 END),0)
             FROM tool_loop_probe
             WHERE created_at >= ?1 AND (?2 IS NULL OR agent_id = ?2)",
            params![cutoff, agent],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .map_err(|e| format!("summarize tool_loop_probe: {e}"))?;

    if turns == 0 {
        return Ok(ProbeSummary::default());
    }

    let mut stmt = conn
        .prepare(
            "SELECT path, COUNT(*) FROM tool_loop_probe
             WHERE created_at >= ?1 AND (?2 IS NULL OR agent_id = ?2)
             GROUP BY path ORDER BY COUNT(*) DESC",
        )
        .map_err(|e| format!("prepare path breakdown: {e}"))?;
    let by_path = stmt
        .query_map(params![cutoff, agent], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })
        .map_err(|e| format!("path breakdown: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("path breakdown rows: {e}"))?;

    let t = turns as f64;
    let cache_denom = billed.saturating_add(cache_read);
    Ok(ProbeSummary {
        turns,
        agents,
        avg_provider_calls: sum_calls as f64 / t,
        max_provider_calls: max_calls,
        avg_schema_tokens: sum_schema / turns,
        max_schema_tokens: max_schema,
        avg_tool_defs: sum_defs / turns,
        schema_share: sum_share / t,
        billed_input_tokens: billed,
        cache_read_tokens: cache_read,
        cache_hit_rate: if cache_denom == 0 {
            0.0
        } else {
            cache_read as f64 / cache_denom as f64
        },
        cache_reporting_share: reporting_turns as f64 / t,
        by_path,
    })
}

// ---------------------------------------------------------------------------
// Report rendering (zh-TW, CLI)
// ---------------------------------------------------------------------------

/// Render the human-readable gate report printed by `duduclaw cost tool-loop`.
pub fn render_report(s: &ProbeSummary, report: &GateReport, days: u64) -> String {
    let mut out = String::new();
    out.push_str("Code Mode Phase 0 量測閘（設計文件 commercial/docs/DESIGN-code-mode-2026-08.md §8.1）\n");
    out.push_str(&format!("視窗：最近 {days} 天\n\n"));

    if s.turns == 0 {
        out.push_str("受益路徑（openai_compat / direct_api / local_inference）在此視窗內沒有任何 tool-loop 輪次。\n");
        out.push_str("這不是錯誤：設計文件 §1.5／R1 已載明本機受益路徑用量為零。\n\n");
        out.push_str(&format!(
            "判定：{}  {}\n",
            report.verdict.as_str(),
            report.verdict.action_zh()
        ));
        return out;
    }

    out.push_str("── 量測結果 ──\n");
    out.push_str(&format!(
        "  輪次 / agent 數      : {} turns / {} agents\n",
        s.turns, s.agents
    ));
    out.push_str(&format!(
        "  provider 呼叫        : 平均 {:.2} 次／輪（最高 {}）\n",
        s.avg_provider_calls, s.max_provider_calls
    ));
    out.push_str(&format!(
        "  工具 schema          : 平均 {} tokens／次呼叫（最高 {}），平均 {} 個工具\n",
        s.avg_schema_tokens, s.max_schema_tokens, s.avg_tool_defs
    ));
    out.push_str(&format!(
        "  schema 佔比          : 首次呼叫輸入的 {:.1}%\n",
        s.schema_share * 100.0
    ));
    out.push_str(&format!(
        "  cache 命中           : {:.1}%（cache_read {} / 帳單 input {}）\n",
        s.cache_hit_rate * 100.0,
        s.cache_read_tokens,
        s.billed_input_tokens
    ));
    out.push_str(&format!(
        "  cache 回報覆蓋率     : {:.0}% 的輪次收到 provider 的 cache 欄位\n",
        s.cache_reporting_share * 100.0
    ));
    if !s.by_path.is_empty() {
        let paths: Vec<String> = s
            .by_path
            .iter()
            .map(|(p, n)| format!("{p}={n}"))
            .collect();
        out.push_str(&format!("  路徑分布             : {}\n", paths.join("  ")));
    }

    out.push_str("\n── 判準（§8.1）──\n");
    for c in &report.checks {
        out.push_str(&format!(
            "  [{}] {} {} — {}\n",
            if c.passed { "PASS" } else { "FAIL" },
            c.id,
            c.label,
            c.detail
        ));
    }

    if let Some(w) = &report.cache_signal_warning {
        out.push_str(&format!("\n⚠ {w}\n"));
    }

    out.push_str(&format!(
        "\n判定：{}\n動作：{}\n",
        report.verdict.as_str(),
        report.verdict.action_zh()
    ));
    out.push_str(
        "\n估算說明：schema tokens 為 CJK-aware 啟發式（chars÷4），非 provider 真 tokenizer；\n\
         量級可用，精確值不可信。cache 數字為各 provider 自報的 usage 欄位。\n",
    );
    out
}

/// Machine-readable twin of [`render_report`].
pub fn report_json(s: &ProbeSummary, report: &GateReport, days: u64) -> serde_json::Value {
    serde_json::json!({
        "window_days": days,
        "measurements": {
            "turns": s.turns,
            "agents": s.agents,
            "avg_provider_calls": s.avg_provider_calls,
            "max_provider_calls": s.max_provider_calls,
            "avg_schema_tokens": s.avg_schema_tokens,
            "max_schema_tokens": s.max_schema_tokens,
            "avg_tool_defs": s.avg_tool_defs,
            "schema_share": s.schema_share,
            "billed_input_tokens": s.billed_input_tokens,
            "cache_read_tokens": s.cache_read_tokens,
            "cache_hit_rate": s.cache_hit_rate,
            "cache_reporting_share": s.cache_reporting_share,
            "by_path": s.by_path.iter().map(|(p, n)| serde_json::json!({"path": p, "turns": n})).collect::<Vec<_>>(),
        },
        "thresholds": {
            "G0_min_turns": GATE_MIN_TURNS,
            "G1_schema_tokens": GATE_SCHEMA_TOKENS,
            "G2_avg_provider_calls": GATE_AVG_PROVIDER_CALLS,
            "G3_cache_absorbed": GATE_CACHE_ABSORBED,
        },
        "checks": report.checks.iter().map(|c| serde_json::json!({
            "id": c.id, "label": c.label, "detail": c.detail, "passed": c.passed,
        })).collect::<Vec<_>>(),
        "cache_signal_warning": report.cache_signal_warning,
        "verdict": report.verdict.as_str(),
        "action": report.verdict.action_zh(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_llm::{ChatMessage, ContentPart, NormalizedUsage, StopReason, SystemBlock, ToolDef};

    fn summary(turns: u64, calls: f64, schema: u64, cache: f64) -> ProbeSummary {
        ProbeSummary {
            turns,
            agents: 1,
            avg_provider_calls: calls,
            max_provider_calls: calls.ceil() as u64,
            avg_schema_tokens: schema,
            max_schema_tokens: schema,
            avg_tool_defs: 213,
            schema_share: 0.4,
            billed_input_tokens: 1_000,
            cache_read_tokens: 0,
            cache_hit_rate: cache,
            cache_reporting_share: 1.0,
            by_path: vec![("openai_compat".into(), turns)],
        }
    }

    #[test]
    fn gate_refuses_to_conclude_below_the_sample_floor() {
        // The design's own n=2 anecdote must NOT read as a decision.
        let s = summary(2, 9.0, 25_000, 0.0);
        let r = evaluate_gate(&s);
        assert_eq!(r.verdict, GateVerdict::InsufficientData);
        assert!(!r.checks[0].passed, "G0 must fail at n=2");
        // G1/G2 still evaluate and print — the numbers are shown, just not acted on.
        assert!(r.checks[1].passed && r.checks[2].passed);
    }

    #[test]
    fn gate_proceeds_when_schema_is_large_even_if_rounds_are_few() {
        let s = summary(GATE_MIN_TURNS, 1.5, GATE_SCHEMA_TOKENS + 1, 0.0);
        let r = evaluate_gate(&s);
        assert_eq!(r.verdict, GateVerdict::Proceed);
    }

    #[test]
    fn gate_proceeds_when_rounds_are_many_even_if_schema_is_small() {
        let s = summary(GATE_MIN_TURNS, GATE_AVG_PROVIDER_CALLS + 0.01, 100, 0.0);
        let r = evaluate_gate(&s);
        assert_eq!(r.verdict, GateVerdict::Proceed);
    }

    #[test]
    fn cache_veto_overrides_both_positive_criteria() {
        // §7.1's finding, applied to the compat path: if the implicit prefix
        // cache already absorbs the cost, big schema + many rounds is moot.
        let s = summary(500, 9.0, 25_000, GATE_CACHE_ABSORBED);
        let r = evaluate_gate(&s);
        assert_eq!(r.verdict, GateVerdict::RejectCacheAbsorbed);
        assert!(!r.checks[3].passed, "G3 veto fired ⇒ the check reads FAIL");
    }

    #[test]
    fn gate_rejects_when_nothing_clears() {
        let s = summary(GATE_MIN_TURNS, 1.2, 500, 0.0);
        assert_eq!(evaluate_gate(&s).verdict, GateVerdict::RejectBelowThreshold);
    }

    #[test]
    fn sparse_cache_reporting_raises_an_explicit_warning() {
        let mut s = summary(GATE_MIN_TURNS, 9.0, 25_000, 0.0);
        s.cache_reporting_share = 0.0;
        let r = evaluate_gate(&s);
        // Verdict still Proceed, but the report says the G3 zero is untrustworthy.
        assert_eq!(r.verdict, GateVerdict::Proceed);
        assert!(r.cache_signal_warning.is_some());
    }

    // ── decorator ───────────────────────────────────────────────────────────

    struct Scripted {
        responses: std::sync::Mutex<Vec<ChatResponse>>,
    }

    #[async_trait]
    impl ChatProvider for Scripted {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
            let mut g = self.responses.lock().unwrap();
            if g.is_empty() {
                return Err(LlmError::Parse("exhausted".into()));
            }
            Ok(g.remove(0))
        }
        async fn stream(
            &self,
            _req: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::Parse("unused".into()))
        }
    }

    fn resp(input: u64, cache: u64) -> ChatResponse {
        ChatResponse {
            parts: vec![ContentPart::Text("ok".into())],
            stop: StopReason::EndTurn,
            usage: NormalizedUsage {
                input_tokens: input,
                cache_read_tokens: cache,
                ..Default::default()
            },
            model_used: "deepseek-v3.2".into(),
            provider: "deepseek".into(),
        }
    }

    fn req_with_tools(n: usize) -> ChatRequest {
        let mut req = ChatRequest::new("deepseek/deepseek-v3.2");
        req.system.push(SystemBlock::uncached("s".repeat(400)));
        req.messages.push(ChatMessage::user("hi"));
        for i in 0..n {
            req.tools.push(ToolDef {
                name: format!("tool_{i}"),
                description: "d".repeat(400),
                input_schema: serde_json::json!({"type": "object"}),
            });
        }
        req
    }

    #[tokio::test]
    async fn probe_counts_rounds_schema_and_cache_without_altering_responses() {
        let inner = Scripted {
            responses: std::sync::Mutex::new(vec![resp(100, 40), resp(120, 60)]),
        };
        let probe = ToolLoopProbe::new(&inner);
        let req = req_with_tools(3);

        let r1 = probe.complete(&req).await.unwrap();
        let r2 = probe.complete(&req).await.unwrap();
        // Passthrough: responses reach the caller untouched.
        assert_eq!(r1.usage.input_tokens, 100);
        assert_eq!(r2.usage.cache_read_tokens, 60);

        let obs = probe.finish("agent-x", ProbePath::OpenAiCompat, "deepseek-v3.2");
        assert_eq!(obs.provider_calls, 2);
        assert_eq!(obs.tool_defs, 3);
        assert_eq!(obs.schema_tokens, req.estimate_tool_schema_tokens());
        // First-call denominator is the FULL input estimate, so the schema is
        // strictly a part of it.
        assert_eq!(obs.first_call_input_tokens, req.estimate_input_tokens());
        assert!(obs.first_call_input_tokens > obs.schema_tokens);
        assert_eq!(obs.billed_input_tokens, 220);
        assert_eq!(obs.cache_read_tokens, 100);
        assert_eq!(obs.cache_reporting_calls, 2);
    }

    #[tokio::test]
    async fn probe_records_a_partial_turn_when_the_provider_errors() {
        // Failure is information: a turn that died mid-loop still measured
        // real rounds, and must not be silently dropped.
        let inner = Scripted {
            responses: std::sync::Mutex::new(vec![resp(100, 0)]),
        };
        let probe = ToolLoopProbe::new(&inner);
        let req = req_with_tools(1);
        assert!(probe.complete(&req).await.is_ok());
        assert!(probe.complete(&req).await.is_err());

        let obs = probe.finish("agent-x", ProbePath::DirectApi, "m");
        // Both attempts counted; only the successful one billed.
        assert_eq!(obs.provider_calls, 2);
        assert_eq!(obs.billed_input_tokens, 100);
        assert_eq!(obs.cache_reporting_calls, 0);
    }

    // ── store round-trip ────────────────────────────────────────────────────

    #[test]
    fn record_and_summarize_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        for i in 0..4u64 {
            record(
                home,
                &ToolLoopObservation {
                    agent_id: format!("agent-{}", i % 2),
                    path: ProbePath::OpenAiCompat,
                    model: "deepseek-v3.2".into(),
                    provider_calls: 2 + i,
                    tool_defs: 213,
                    schema_tokens: 25_000,
                    first_call_input_tokens: 50_000,
                    billed_input_tokens: 1_000,
                    cache_read_tokens: 0,
                    cache_reporting_calls: 0,
                },
            )
            .unwrap();
        }

        let s = summarize(home, 30, None).unwrap();
        assert_eq!(s.turns, 4);
        assert_eq!(s.agents, 2);
        // (2+3+4+5)/4
        assert!((s.avg_provider_calls - 3.5).abs() < 1e-9);
        assert_eq!(s.max_provider_calls, 5);
        assert_eq!(s.avg_schema_tokens, 25_000);
        assert!((s.schema_share - 0.5).abs() < 1e-9);
        assert_eq!(s.cache_hit_rate, 0.0);
        assert_eq!(s.cache_reporting_share, 0.0);
        assert_eq!(s.by_path, vec![("openai_compat".to_string(), 4)]);

        // Agent filter narrows the window.
        let one = summarize(home, 30, Some("agent-0")).unwrap();
        assert_eq!(one.turns, 2);
        assert_eq!(one.agents, 1);
    }

    #[test]
    fn render_report_prints_every_criterion_with_its_actual_vs_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // 40 turns shaped like the design's §7.1 anecdote, but at a sample
        // size that clears G0 — the "Code Mode looks worth it" scenario.
        for i in 0..40u64 {
            record(
                home,
                &ToolLoopObservation {
                    agent_id: "grok-analyst".into(),
                    path: ProbePath::OpenAiCompat,
                    model: "grok-4.1-fast".into(),
                    provider_calls: 5 + (i % 3),
                    tool_defs: 213,
                    schema_tokens: 25_000,
                    first_call_input_tokens: 41_000,
                    billed_input_tokens: 180_000,
                    cache_read_tokens: 20_000,
                    cache_reporting_calls: 3,
                },
            )
            .unwrap();
        }
        let s = summarize(home, 30, None).unwrap();
        let r = evaluate_gate(&s);
        let text = render_report(&s, &r, 30);
        println!("{text}");

        for id in ["G0", "G1", "G2", "G3"] {
            assert!(text.contains(id), "criterion {id} missing from report");
        }
        assert!(text.contains("PROCEED"));
        assert_eq!(r.verdict, GateVerdict::Proceed);
        // JSON twin carries the same verdict and every threshold.
        let j = report_json(&s, &r, 30);
        assert_eq!(j["verdict"], "PROCEED");
        assert_eq!(j["thresholds"]["G1_schema_tokens"], GATE_SCHEMA_TOKENS);
        assert_eq!(j["checks"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn summarize_on_an_empty_store_reports_zero_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = summarize(dir.path(), 30, None).unwrap();
        assert_eq!(s, ProbeSummary::default());
        let r = evaluate_gate(&s);
        assert_eq!(r.verdict, GateVerdict::InsufficientData);
        // The rendered report must say "zero usage", never fabricate a decision.
        let text = render_report(&s, &r, 30);
        assert!(text.contains("沒有任何 tool-loop 輪次"));
        assert!(text.contains("INSUFFICIENT_DATA"));
    }
}
