//! Loop result / telemetry types: the CCR delivery guards, the saved-result
//! record, the counters and the per-call ledger entry.
//! Moved verbatim out of `tool_loop.rs`.

use super::*;

/// [`run_tool_loop`] result plus argument-level provenance findings (S2).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolLoopOutcome {
    pub response: ChatResponse,
    /// Counters from the complete provider/tool loop, including intermediate
    /// tool-use rounds that are absent from `response.usage`.
    pub telemetry: ToolLoopTelemetry,
    /// Every taint hit / overflow event on a *sensitive* tool call, in
    /// dispatch order. Empty when the policy is `Off` or nothing flagged.
    pub provenance_flags: Vec<ProvenanceFlag>,
    /// WP-A5 (design `commercial/docs/design-task-forward-model-2026-08-06.md`
    /// §5.3/§9), extended R1 (2026-08,
    /// `wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md`
    /// §6): every tool call dispatched across the whole loop, in dispatch
    /// order. `success = false` for a call whose outcome carried
    /// `is_error = true` — this covers an executor error result, a dispatch
    /// failure fed back as an error result, AND a provenance-blocked call
    /// (never dispatched, but still an "attempted, refused" tool use).
    /// Runtime-neutral by construction (no call id) — callers merge this
    /// into `duduclaw-gateway::runtime::NativeToolEvent` for the A3
    /// forward-model's `Full` observation fidelity AND (R1) the B3
    /// grounding pre-check.
    pub tool_calls: Vec<LoopToolCall>,
    /// Opaque CCR handles committed for successful tool results in this loop.
    /// Callers must recheck the current scope and source policy before using
    /// a handle in a later turn; this is provenance, not authorization.
    pub ccr_saved_results: Vec<CcrSavedResult>,
    /// Source delivery leases for CCR originals read during this loop.
    /// The caller must retain these until the reply has been sent.
    pub ccr_delivery_guards: CcrDeliveryGuards,
}

#[derive(Debug, Clone, Default)]
pub struct CcrDeliveryGuards(Vec<std::sync::Arc<dyn crate::ccr::CcrDeliveryLease>>);

impl CcrDeliveryGuards {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Synchronous revalidation. Each guard opens SQLite (3-4 connections per
    /// entry guard) and source authorities may additionally read files, so
    /// this must NOT run on a tokio worker thread — use [`Self::still_valid`]
    /// from async code and keep this for sync callers and tests.
    pub fn still_valid_blocking(&self) -> bool {
        self.0.iter().all(|guard| guard.still_valid())
    }

    /// Revalidate off the async reactor. An empty guard set answers without
    /// touching the blocking pool; a panicked or cancelled revalidation fails
    /// closed (`false`), never "assume still valid".
    pub async fn still_valid(&self) -> bool {
        if self.0.is_empty() {
            return true;
        }
        let guards = self.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::spawn_blocking(move || guards.still_valid_blocking())
                .await
                .unwrap_or(false),
            // No tokio runtime (a `block_on` from a sync CLI path): there is
            // no reactor to protect, so run it inline rather than panic.
            Err(_) => guards.still_valid_blocking(),
        }
    }

    pub fn push(&mut self, guard: std::sync::Arc<dyn crate::ccr::CcrDeliveryLease>) {
        self.0.push(guard);
    }
}

// The guards are lifetime protection, not part of the observable loop result.
impl PartialEq for CcrDeliveryGuards {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcrSavedResult {
    pub scope: CcrScope,
    pub source_tool: String,
    pub source_call_id: String,
    pub id: String,
    pub original_bytes: usize,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolLoopTelemetry {
    pub provider_rounds: u64,
    /// Nonzero usage was reported in these rounds; other rounds may be
    /// unmetered, so summed usage must not be labeled a complete bill.
    pub usage_reported_rounds: u64,
    pub provider_usage: crate::types::NormalizedUsage,
    pub ccr_compressed_results: u64,
    pub ccr_original_bytes: u64,
    pub ccr_delivered_bytes: u64,
    pub ccr_find_attempts: u64,
    pub ccr_find_hits: u64,
    pub ccr_find_misses: u64,
    /// `duduclaw_ccr_find` calls refused by [`CCR_FIND_MAX_CALLS_PER_LOOP`].
    /// Persisted as a `ccr_loop_telemetry` column since CCR schema version 2
    /// (W3-2 #3): a budget the model keeps hitting is exactly the signal the
    /// dashboard needs, and log-only left it invisible to every operator.
    pub ccr_find_rate_limited: u64,
    pub ccr_retrieve_attempts: u64,
    pub ccr_retrieve_successes: u64,
    pub ccr_retrieve_misses: u64,
    pub ccr_retrieved_bytes: u64,
    pub elapsed_millis: u128,
}

impl ToolLoopTelemetry {
    pub(super) fn record_provider(&mut self, response: &ChatResponse) {
        self.provider_rounds = self.provider_rounds.saturating_add(1);
        let usage = response.usage;
        if usage.input_tokens > 0
            || usage.output_tokens > 0
            || usage.cache_read_tokens > 0
            || usage.cache_write_tokens > 0
            || usage.reasoning_tokens > 0
        {
            self.usage_reported_rounds = self.usage_reported_rounds.saturating_add(1);
        }
        self.provider_usage = self.provider_usage.saturating_add(&usage);
    }
}

/// Per-scope `duduclaw_ccr_find` budget for one tool loop. `find` is a
/// model-visible tool whose SQL scans every original in the caller's scope
/// (each up to 2 MiB), and the existing `limit.clamp(1,5)` caps a single
/// answer's size, not how often the model may ask. One loop is scoped to one
/// `CcrScope`, so a per-loop counter IS the per-scope cap. Over budget ⇒ an
/// explicit `is_error` tool result so the model re-plans instead of retrying
/// into an unbounded scan.
pub const CCR_FIND_MAX_CALLS_PER_LOOP: u64 = 8;
pub(super) const CCR_FIND_RATE_LIMITED_MESSAGE: &str =
    "CCR search budget exhausted for this turn (max 8 duduclaw_ccr_find calls); \
     answer from the results already retrieved or ask the user to narrow the request";

pub(super) const CCR_TELEMETRY_PENDING_LIMIT: usize = 16;
static CCR_TELEMETRY_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(super) fn try_ccr_telemetry_permit(pool: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    pool.clone().try_acquire_owned().ok()
}

pub(super) fn finish_tool_loop(
    response: ChatResponse,
    provenance_flags: Vec<ProvenanceFlag>,
    tool_calls: Vec<LoopToolCall>,
    ccr_saved_results: Vec<CcrSavedResult>,
    ccr_delivery_guards: CcrDeliveryGuards,
    mut telemetry: ToolLoopTelemetry,
    started: Instant,
    ccr: Option<&CcrRuntime>,
) -> ToolLoopOutcome {
    telemetry.elapsed_millis = started.elapsed().as_millis();
    if let Some(runtime) = ccr {
        tracing::info!(
            event = "ccr_loop_metrics",
            provider_rounds = telemetry.provider_rounds,
            usage_reported_rounds = telemetry.usage_reported_rounds,
            input_tokens = telemetry.provider_usage.input_tokens,
            output_tokens = telemetry.provider_usage.output_tokens,
            cache_read_tokens = telemetry.provider_usage.cache_read_tokens,
            cache_write_tokens = telemetry.provider_usage.cache_write_tokens,
            ccr_compressed_results = telemetry.ccr_compressed_results,
            ccr_original_bytes = telemetry.ccr_original_bytes,
            ccr_delivered_bytes = telemetry.ccr_delivered_bytes,
            ccr_find_attempts = telemetry.ccr_find_attempts,
            ccr_find_hits = telemetry.ccr_find_hits,
            ccr_find_misses = telemetry.ccr_find_misses,
            ccr_find_rate_limited = telemetry.ccr_find_rate_limited,
            ccr_retrieve_attempts = telemetry.ccr_retrieve_attempts,
            ccr_retrieve_successes = telemetry.ccr_retrieve_successes,
            ccr_retrieve_misses = telemetry.ccr_retrieve_misses,
            ccr_retrieved_bytes = telemetry.ccr_retrieved_bytes,
            elapsed_millis = telemetry.elapsed_millis,
            "CCR tool-loop metrics"
        );
        let pool = CCR_TELEMETRY_PERMITS
            .get_or_init(|| Arc::new(Semaphore::new(CCR_TELEMETRY_PENDING_LIMIT)));
        if let Some(permit) = try_ccr_telemetry_permit(pool) {
            let store = runtime.store.clone();
            let scope = runtime.scope.clone();
            let counters = telemetry.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                if let Err(error) = store.record_loop_telemetry(&scope, &counters) {
                    tracing::warn!(error = %error, "CCR loop telemetry write failed");
                }
            });
        } else {
            tracing::warn!(
                event = "ccr_loop_telemetry_dropped",
                reason = "busy",
                "CCR loop telemetry queue full"
            );
        }
    }
    ToolLoopOutcome {
        response,
        telemetry,
        provenance_flags,
        tool_calls,
        ccr_saved_results,
        ccr_delivery_guards,
    }
}

/// One tool call the loop dispatched (or refused to dispatch), carrying
/// masked+capped evidence text for downstream consumers (R1's B3 grounding
/// pre-check chief among them). Masking happens HERE — inside
/// `duduclaw-llm`, which already depends on `duduclaw-security` for the
/// `PolicyExecutor` — so no unmasked tool text ever crosses into
/// `duduclaw-gateway::runtime::NativeToolEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopToolCall {
    pub tool_name: String,
    pub success: bool,
    /// Masked + CJK-safe-truncated tool result text (the `ToolOutcome`
    /// content, or the refusal/error text on a blocked/failed call). `None`
    /// only when the content was empty/all-whitespace after masking — never
    /// omitted merely because the call failed (an error's text is still
    /// useful context, same convention as the MCP audit trail's
    /// `append_tool_call_with_input`; `check_grounded` already excludes
    /// `is_error` evidence from grounding on its own).
    pub result_text: Option<String>,
    /// Masked + CJK-safe-truncated serialized tool-call arguments.
    pub input_text: Option<String>,
}
