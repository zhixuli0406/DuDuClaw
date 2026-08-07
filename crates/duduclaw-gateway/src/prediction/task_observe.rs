//! WP-A6 — observation layer: builds a [`TaskObservation`] from
//! runtime-neutral, programmatic evidence only.
//!
//! Hard constraint 3 (design §8.1): observation reads **only** program­matic
//! traces (`tool_calls.jsonl`), never the agent's own `result_summary`
//! self-report. When no evidence is reachable, [`observe_round`] returns
//! `fidelity = ObservationFidelity::None` — it never guesses (opus-playbook
//! §5: "永遠不降到猜一個值").
//!
//! **`Full` fidelity** (WP-A4/A5/T10, design §5.3): once a caller passes
//! `native_evidence: Some(...)` — meaning the WP-A4/A5/T10 native-tool
//! collector actually ran for this round — [`observe_round`] merges it with
//! whatever MCP-audit evidence is available: tool classes are unioned, call
//! counts and error counts take the max of the two sources (not the sum —
//! an MCP tool call is visible on BOTH sides, so summing would double-count
//! it). `native_evidence: None` (the collector didn't run — most rounds
//! today, per design §8.2's runtime × capability matrix) falls back to the
//! original `McpOnly`/`None` behavior unchanged.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use duduclaw_core::types::RuntimeType;
use tracing::warn;

use crate::runtime::NativeToolEvent;
use crate::tool_activity::read_tool_activity_records;

use super::task_forward::{ArtifactShape, ObservationFidelity, ObservedOutcome, TaskObservation};
use super::tool_class::{ToolClass, other_ratio_exceeds_threshold};

// ═══════════════════════════════════════════════════════════════════════
// WP-A4/A5/T10 bridge: dispatcher (producer) → dispatch_engine (consumer)
// ═══════════════════════════════════════════════════════════════════════
//
// `dispatcher.rs` (right after a goal-loop dispatch call returns) and
// `dispatch_engine.rs::settle_forward_model` (on its own independent poll
// tick — design §4.2 confirms predict/dispatch and settle/review are two
// separate scheduler loops with no direct call relationship) need an
// in-memory channel that survives the gap between "a dispatch finished" and
// "that round got reviewed". Design §7.1 rules out a new SQL store for this
// ("不落地" — the collector is meant to ride the call, not land on disk);
// this process-lifetime map is the alternative that stays true to that
// constraint. An entry lost to a gateway restart between dispatch and
// settle simply degrades that round to `McpOnly` (pre-WP-A4 behavior),
// never a wrong guess.
//
// Bounded per hard constraint 4 ("沒有剪枝的自我演化必然膨脹"): capped at
// [`NATIVE_EVIDENCE_CAP`] entries with FIFO eviction of the oldest. A goal
// loop produces at most a handful of rounds per minute (design R1: "goal
// loop 的輪次產生速率遠低於對話層"), so this is generous headroom, not a
// real limit in practice.

const NATIVE_EVIDENCE_CAP: usize = 500;

struct NativeEvidenceStore {
    order: VecDeque<(String, u32)>,
    map: HashMap<(String, u32), Vec<NativeToolEvent>>,
}

impl NativeEvidenceStore {
    fn new() -> Self {
        Self { order: VecDeque::new(), map: HashMap::new() }
    }

    fn insert(&mut self, task_id: String, round: u32, events: Vec<NativeToolEvent>) {
        let key = (task_id, round);
        if !self.map.contains_key(&key) {
            if self.order.len() >= NATIVE_EVIDENCE_CAP {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
            self.order.push_back(key.clone());
        }
        self.map.insert(key, events);
    }

    fn take(&mut self, task_id: &str, round: u32) -> Option<Vec<NativeToolEvent>> {
        let key = (task_id.to_string(), round);
        let out = self.map.remove(&key);
        if out.is_some() {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        }
        out
    }
}

fn native_evidence_store() -> &'static Mutex<NativeEvidenceStore> {
    static STORE: OnceLock<Mutex<NativeEvidenceStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(NativeEvidenceStore::new()))
}

/// Producer-side entry point: called by `dispatcher.rs` right after a
/// goal-loop dispatch call returns, with whatever the
/// [`crate::runtime::NATIVE_TOOL_COLLECTOR`] task-local accumulated during
/// that call (possibly empty — a genuinely tool-free round is still `Full`
/// fidelity once merged, see [`observe_round`]). Best-effort: the mutex is
/// only held for a bounded insert, never blocks the dispatch response.
pub fn record_native_evidence(task_id: &str, round: u32, events: Vec<NativeToolEvent>) {
    if let Ok(mut store) = native_evidence_store().lock() {
        store.insert(task_id.to_string(), round, events);
    }
}

/// Consumer-side entry point: called by
/// `dispatch_engine.rs::settle_forward_model` once per settled round.
/// Removes (not just reads) the entry — a round is only ever settled once,
/// and this keeps the store from growing on rounds that DO get consumed.
pub fn take_native_evidence(task_id: &str, round: u32) -> Option<Vec<NativeToolEvent>> {
    native_evidence_store()
        .lock()
        .ok()
        .and_then(|mut store| store.take(task_id, round))
}

/// Build one round's observation.
///
/// Source priority (design §5.3): `<home>/tool_calls.jsonl` is always read
/// as the MCP-audit side (the "today" column of design §8.2's runtime ×
/// capability matrix — every runtime, including Claude on the goal-loop
/// dispatch path, is `McpOnly` absent a native collector). When
/// `native_evidence` is `Some(...)` (WP-A4/A5/T10 actually ran for this
/// round), the two sources are merged into a `Full` observation — see the
/// module docs. `home_dir = None`, a missing/unreadable file, AND no native
/// evidence together degrade to `fidelity = None` rather than fabricating a
/// plausible-looking empty observation with a misleadingly higher fidelity.
///
/// `deterministic_passed` (the outcome-spec zero-cost check result from
/// `dispatch_engine.rs`, when available) is accepted for forward
/// compatibility but is not yet consulted by this version's artifact
/// inference — the caller's `outcome`/`ArtifactShape` classification
/// already folds it in upstream (WP-A9 scope). Kept in the signature now so
/// that wiring doesn't require changing this function's shape later.
#[allow(clippy::too_many_arguments)]
pub fn observe_round(
    home_dir: Option<&Path>,
    agent_id: &str,
    runtime: RuntimeType,
    task_id: &str,
    round: u32,
    window: (&str, &str),
    outcome: ObservedOutcome,
    observed_artifact: ArtifactShape,
    _deterministic_passed: Option<bool>,
    native_evidence: Option<&[NativeToolEvent]>,
) -> TaskObservation {
    let (since, until) = window;

    // WP-A4/A5/T10: a native-tool collector actually ran for this round —
    // merge it with whatever MCP-audit evidence is available. This branch
    // NEVER degrades to McpOnly/None: the collector having run at all
    // (even, and especially, if it saw zero native tool calls) is itself
    // the Full signal — "we looked at everything, not just the MCP
    // subset, and this is what we found" is strictly more information
    // than the McpOnly branch below ever has.
    if let Some(native) = native_evidence {
        let mcp_records = home_dir
            .map(|home| read_tool_activity_records(home, agent_id, since, until))
            .unwrap_or_default();
        return observe_full(
            agent_id,
            runtime,
            task_id,
            round,
            (since, until),
            outcome,
            observed_artifact,
            &mcp_records,
            native,
        );
    }

    let Some(home) = home_dir else {
        warn!(
            task_id, round, agent_id,
            "[unobservable: {task_id} round={round}: no home_dir configured for this agent]"
        );
        return unobservable(task_id, agent_id, round, window, runtime);
    };

    let records = read_tool_activity_records(home, agent_id, since, until);
    if records.is_empty() {
        // Ambiguous by design: "no records" could mean the file is
        // missing, the window had genuinely zero tool activity, or the
        // agent used only native tools this version can't see. All three
        // collapse to the same honest answer under McpOnly: no evidence to
        // build a McpOnly observation from either — degrade to None rather
        // than assert "the agent used zero tools" as a claim.
        warn!(
            task_id, round, agent_id,
            "[unobservable: {task_id} round={round}: no tool_calls.jsonl evidence in window {since}..{until}]"
        );
        return unobservable(task_id, agent_id, round, window, runtime);
    }

    let classes: Vec<ToolClass> = records
        .iter()
        .map(|r| ToolClass::classify(runtime, &r.tool_name))
        .collect();
    if other_ratio_exceeds_threshold(&classes) {
        warn!(
            task_id, round, agent_id,
            other_count = classes.iter().filter(|c| **c == ToolClass::Other).count(),
            total = classes.len(),
            "task_observe: Other tool-class ratio exceeds warn threshold — ToolClass mapping table may be missing entries (R4)"
        );
    }
    let observed_tool_classes = classes.into_iter().collect();
    let observed_calls = records.len() as u32;
    let observed_errors = records.iter().filter(|r| !r.success).count() as u32;

    TaskObservation {
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        round,
        observed_tool_classes,
        observed_calls,
        observed_errors,
        observed_outcome: outcome,
        observed_artifact,
        fidelity: ObservationFidelity::McpOnly,
        window: (since.to_string(), until.to_string()),
        runtime: runtime.as_str().to_string(),
    }
}

/// WP-A4/A5/T10 merge (design §5.3): union of tool classes, `max` (not sum)
/// of call/error counts across the two evidence sources. Always `Full`
/// fidelity — the collector having run is itself the signal, independent
/// of whether either source observed anything.
#[allow(clippy::too_many_arguments)]
fn observe_full(
    agent_id: &str,
    runtime: RuntimeType,
    task_id: &str,
    round: u32,
    window: (&str, &str),
    outcome: ObservedOutcome,
    observed_artifact: ArtifactShape,
    mcp_records: &[crate::tool_activity::ToolActivityRecord],
    native: &[NativeToolEvent],
) -> TaskObservation {
    let (since, until) = window;

    let mut classes: Vec<ToolClass> = mcp_records
        .iter()
        .map(|r| ToolClass::classify(runtime, &r.tool_name))
        .collect();
    classes.extend(native.iter().map(|e| ToolClass::classify(runtime, &e.tool_name)));
    if other_ratio_exceeds_threshold(&classes) {
        warn!(
            task_id, round, agent_id,
            other_count = classes.iter().filter(|c| **c == ToolClass::Other).count(),
            total = classes.len(),
            "task_observe: Other tool-class ratio exceeds warn threshold (Full merge) — ToolClass mapping table may be missing entries (R4)"
        );
    }
    let observed_tool_classes = classes.into_iter().collect();

    let mcp_calls = mcp_records.len() as u32;
    let native_calls = native.len() as u32;
    // design §5.3: max, not sum — an MCP tool call is visible in BOTH
    // sources (the native collector sees every tool_use block, MCP or not),
    // so summing would double-count it.
    let observed_calls = mcp_calls.max(native_calls);

    let mcp_errors = mcp_records.iter().filter(|r| !r.success).count() as u32;
    let native_errors = native.iter().filter(|e| !e.success).count() as u32;
    let observed_errors = mcp_errors.max(native_errors);

    TaskObservation {
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        round,
        observed_tool_classes,
        observed_calls,
        observed_errors,
        observed_outcome: outcome,
        observed_artifact,
        fidelity: ObservationFidelity::Full,
        window: (since.to_string(), until.to_string()),
        runtime: runtime.as_str().to_string(),
    }
}

fn unobservable(
    task_id: &str,
    agent_id: &str,
    round: u32,
    window: (&str, &str),
    runtime: RuntimeType,
) -> TaskObservation {
    TaskObservation {
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        round,
        observed_tool_classes: Default::default(),
        observed_calls: 0,
        observed_errors: 0,
        observed_outcome: ObservedOutcome::Unknown,
        observed_artifact: ArtifactShape::TextOnly,
        fidelity: ObservationFidelity::None,
        window: (window.0.to_string(), window.1.to_string()),
        runtime: runtime.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_round_no_home_dir_is_none_fidelity() {
        let obs = observe_round(
            None,
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.fidelity, ObservationFidelity::None);
        assert_eq!(obs.observed_outcome, ObservedOutcome::Unknown);
        assert!(obs.observed_tool_classes.is_empty());
    }

    #[test]
    fn observe_round_missing_file_is_none_fidelity() {
        let dir = tempfile::tempdir().unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.fidelity, ObservationFidelity::None);
    }

    #[test]
    fn observe_round_empty_window_is_none_fidelity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T05:00:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"Bash\",\"success\":true}\n",
        )
        .unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"), // outside the recorded timestamp
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.fidelity, ObservationFidelity::None);
    }

    #[test]
    fn observe_round_with_evidence_is_mcp_only_never_full() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":true}\n",
                "{\"timestamp\":\"2026-08-06T00:03:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"memory_search\",\"success\":false}\n",
            ),
        )
        .unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.fidelity, ObservationFidelity::McpOnly, "design §8.2: McpOnly is the primary branch, not an exception");
        assert_eq!(obs.observed_calls, 2);
        assert_eq!(obs.observed_errors, 1);
        assert!(obs.observed_tool_classes.contains(&ToolClass::TaskBoard));
        assert!(obs.observed_tool_classes.contains(&ToolClass::Memory));
    }

    #[test]
    fn observe_round_scopes_to_agent_and_other_agents_excluded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"Bash\",\"success\":true}\n",
                "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"other\",\"tool_name\":\"Bash\",\"success\":true}\n",
            ),
        )
        .unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.observed_calls, 1);
    }

    #[test]
    fn observe_round_never_reads_result_summary() {
        // Hard constraint 3 regression: a tool_calls.jsonl line that
        // happens to embed a "result_summary"-shaped key must NOT influence
        // observed_outcome — only the `outcome` parameter (caller-supplied,
        // itself derived from the judge/deterministic chain, never the
        // agent's self-report) does.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"Bash\",\"success\":true,\"result_summary\":\"I definitely succeeded and did everything perfectly\"}\n",
        )
        .unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Rejected, // caller says rejected regardless of the embedded self-report text
            ArtifactShape::TextOnly,
            None,
            None,
        );
        assert_eq!(obs.observed_outcome, ObservedOutcome::Rejected);
    }

    #[test]
    fn observe_round_runtime_agnostic_reads_same_jsonl_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":true}\n",
        )
        .unwrap();
        for runtime in [RuntimeType::Claude, RuntimeType::Codex, RuntimeType::Gemini, RuntimeType::OpenAiCompat] {
            let obs = observe_round(
                Some(dir.path()),
                "agnes",
                runtime,
                "t1",
                1,
                ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
                ObservedOutcome::Accepted,
                ArtifactShape::FileWrite,
                None,
                None,
            );
            assert_eq!(obs.fidelity, ObservationFidelity::McpOnly);
            assert_eq!(obs.runtime, runtime.as_str());
        }
    }

    // ── WP-A4/A5/T10: Full-fidelity merge ──────────────────────────────

    fn native(tool_name: &str, success: bool) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: None,
            input_text: None,
        }
    }

    #[test]
    fn observe_round_native_evidence_present_is_full_even_when_empty() {
        // No MCP records, no home_dir, but the collector ran and saw
        // nothing — this is still Full ("we looked at everything and
        // found nothing"), never a degrade to McpOnly/None.
        let obs = observe_round(
            None,
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::TextOnly,
            None,
            Some(&[]),
        );
        assert_eq!(obs.fidelity, ObservationFidelity::Full);
        assert_eq!(obs.observed_calls, 0);
        assert!(obs.observed_tool_classes.is_empty());
    }

    #[test]
    fn observe_round_full_merge_unions_tool_classes() {
        let dir = tempfile::tempdir().unwrap();
        // MCP side sees a memory_search call; native side sees a Bash call
        // the MCP audit trail can never capture.
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"memory_search\",\"success\":true}\n",
        )
        .unwrap();
        let native_events = vec![native("Bash", true)];
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            Some(&native_events),
        );
        assert_eq!(obs.fidelity, ObservationFidelity::Full);
        assert!(obs.observed_tool_classes.contains(&ToolClass::Memory));
        assert!(obs.observed_tool_classes.contains(&ToolClass::Exec));
    }

    #[test]
    fn observe_round_full_merge_takes_max_not_sum_of_call_counts() {
        let dir = tempfile::tempdir().unwrap();
        // MCP audit and native collector both see the SAME two MCP calls
        // (the native collector captures every tool_use block, MCP or
        // not) — summing would double-count to 4; max keeps it at 2.
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":true}\n",
                "{\"timestamp\":\"2026-08-06T00:03:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_complete\",\"success\":true}\n",
            ),
        )
        .unwrap();
        let native_events = vec![native("mcp__duduclaw__tasks_create", true), native("mcp__duduclaw__tasks_complete", true)];
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            Some(&native_events),
        );
        assert_eq!(obs.observed_calls, 2, "max(2, 2), not sum(2, 2) = 4");
    }

    #[test]
    fn observe_round_full_merge_takes_max_of_call_counts_when_native_is_superset() {
        let dir = tempfile::tempdir().unwrap();
        // MCP audit only sees 1 call; native collector (superset — it also
        // sees native Bash/Read/Write) sees 3. max should pick 3.
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":true}\n",
        )
        .unwrap();
        let native_events =
            vec![native("Bash", true), native("Read", true), native("mcp__duduclaw__tasks_create", true)];
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            Some(&native_events),
        );
        assert_eq!(obs.observed_calls, 3);
    }

    #[test]
    fn observe_round_full_merge_takes_max_of_error_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":false}\n",
        )
        .unwrap();
        // Native side has 2 failures (one of them is the same MCP call, one
        // is a native-only Bash failure the MCP audit never saw).
        let native_events = vec![native("mcp__duduclaw__tasks_create", false), native("Bash", false)];
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            Some(&native_events),
        );
        assert_eq!(obs.observed_errors, 2, "max(1, 2)");
    }

    #[test]
    fn observe_round_native_none_falls_back_to_mcp_only() {
        // native_evidence: None is the "collector didn't run" case (design
        // §8.2 — most rounds today) and must be byte-identical to the
        // pre-WP-A4 McpOnly behavior.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-08-06T00:02:00Z\",\"agent_id\":\"agnes\",\"tool_name\":\"tasks_create\",\"success\":true}\n",
        )
        .unwrap();
        let obs = observe_round(
            Some(dir.path()),
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            None,
        );
        assert_eq!(obs.fidelity, ObservationFidelity::McpOnly);
    }

    #[test]
    fn observe_round_native_evidence_with_no_home_dir_and_no_mcp_records_is_full_not_none() {
        // No home_dir at all (so no MCP side can be read), but the native
        // collector still ran — Full, not None. This is the case a
        // channel-less / home-less agent dispatch with an active collector
        // would hit.
        let native_events = vec![native("Bash", true)];
        let obs = observe_round(
            None,
            "agnes",
            RuntimeType::Claude,
            "t1",
            1,
            ("2026-08-06T00:00:00Z", "2026-08-06T00:10:00Z"),
            ObservedOutcome::Accepted,
            ArtifactShape::FileWrite,
            None,
            Some(&native_events),
        );
        assert_eq!(obs.fidelity, ObservationFidelity::Full);
        assert_eq!(obs.observed_calls, 1);
    }

    // ── WP-A4/A5/T10 bridge store (record_native_evidence / take_native_evidence) ──

    #[test]
    fn native_evidence_bridge_round_trips() {
        let task_id = format!("bridge-test-{}", uuid::Uuid::new_v4());
        record_native_evidence(&task_id, 1, vec![native("Bash", true)]);
        let taken = take_native_evidence(&task_id, 1);
        assert_eq!(taken.map(|v| v.len()), Some(1));
    }

    #[test]
    fn native_evidence_bridge_take_consumes_entry() {
        let task_id = format!("bridge-test-consume-{}", uuid::Uuid::new_v4());
        record_native_evidence(&task_id, 1, vec![native("Bash", true)]);
        assert!(take_native_evidence(&task_id, 1).is_some());
        // Second take on the same key: already consumed.
        assert!(take_native_evidence(&task_id, 1).is_none());
    }

    #[test]
    fn native_evidence_bridge_missing_key_is_none() {
        let task_id = format!("bridge-test-missing-{}", uuid::Uuid::new_v4());
        assert!(take_native_evidence(&task_id, 99).is_none());
    }

    #[test]
    fn native_evidence_bridge_distinguishes_round() {
        let task_id = format!("bridge-test-round-{}", uuid::Uuid::new_v4());
        record_native_evidence(&task_id, 1, vec![native("Bash", true)]);
        record_native_evidence(&task_id, 2, vec![native("Read", true), native("Write", false)]);
        assert_eq!(take_native_evidence(&task_id, 1).map(|v| v.len()), Some(1));
        assert_eq!(take_native_evidence(&task_id, 2).map(|v| v.len()), Some(2));
    }
}
