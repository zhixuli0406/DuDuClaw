//! P3-3 lightweight CEP (Complex Event Processing) — in-process time-window
//! sequence matcher on top of the existing autopilot event bus.
//!
//! ## What this is
//!
//! `autopilot_engine` dispatches on a *single* event: `trigger_event ==
//! event_name && evaluate(conditions, fields)`. This module adds a second,
//! independent dispatch shape — a **temporal pattern** over two events:
//!
//! > event `A` (matching `first`), then within `within_secs` seconds event
//! > `B` (matching `then`) — fire (or, with `negate: true`, fire only if `B`
//! > does **not** show up within the window).
//!
//! `research-os-native-agent-methodology.md` §3.1 / §②-7 point at arXiv
//! 2501.00906 (Autogen+Kafka CEP pipeline) for the *concept* only — that
//! paper's actual application domain is multimedia IoT video querying, not a
//! general CEP framework (`[verified-caveat]`, see the methodology doc's
//! citation table), and the project's explicit decision (§ risk list) is to
//! **not** pull in a streaming platform (Kafka/Flink/Autogen) for this. The
//! pattern matcher here is 100% deterministic Rust — no LLM is ever asked to
//! reason about "did A happen before B" (the same source material warns LLM
//! temporal reasoning is error-prone).
//!
//! ## Design (in-process, not durable)
//!
//! - State (`pending` map) lives entirely in memory. A gateway restart loses
//!   any in-flight sequence windows — this is an accepted trade-off for a
//!   lightweight, dependency-free implementation (documented, not silent).
//!   `events.db` itself is untouched and unread by this module; it already
//!   has its own retention/pruning independent of CEP state.
//! - Every rule with a non-null `sequence` column is evaluated against
//!   **every** event that flows over the autopilot broadcast bus. Rules are
//!   re-read from `AutopilotStore` per event (same pattern
//!   `AutopilotEngine::process_event` already uses for ordinary rules), so a
//!   newly created/edited sequence rule takes effect on the next event
//!   without a restart.
//! - A resolved pattern is turned into a synthetic
//!   [`AutopilotEvent::CepTrigger`] re-broadcast onto the **same** bus this
//!   matcher subscribes to. `AutopilotEngine::process_event` special-cases
//!   that variant: it looks the rule up by id and fires it directly
//!   (circuit breaker, `execute_action`, history/activity — the exact same
//!   tail as an ordinary rule match), so `proactive_notify` still goes
//!   through `ProactiveGate` and nothing bypasses the existing action gates.
//!
//! ## Known limitation (by design, not a gap to silently paper over)
//!
//! `first`/`then` `match` conditions can only compare a field to a **static**
//! literal (the same shape as ordinary autopilot `conditions`) — there is no
//! cross-event correlation (e.g. "the SAME `agent_id` that started the
//! window"). A rule author who needs that must include an explicit
//! `{"field":"agent_id","op":"eq","value":"<literal>"}` in both `first.match`
//! and `then.match`. This mirrors the existing `evaluate()` condition
//! language exactly (no new operators invented) and keeps the matcher's
//! surface area small and auditable.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::autopilot_engine::{AutopilotEvent, CONDITION_OPS, evaluate};
use crate::autopilot_store::AutopilotStore;

// ─── Sequence spec (the `sequence` rule JSON) ──────────────────────────────

/// One side of a sequence pattern — the event name to match plus an
/// (optional) condition against that event's flattened field map. Shape of
/// `match_cond` is identical to an ordinary autopilot rule's `conditions`
/// (`all`/`any`/`{field,op,value}`, `null` = always true).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceStep {
    pub event: String,
    #[serde(rename = "match", default)]
    pub match_cond: Value,
}

/// The `sequence` field of an autopilot rule: "`first` then `then` within
/// `within_secs` seconds" (or, negated, "`first` then NOT `then` within
/// `within_secs` seconds").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceSpec {
    pub first: SequenceStep,
    pub then: SequenceStep,
    pub within_secs: u64,
    #[serde(default)]
    pub negate: bool,
}

/// Event names a `SequenceStep.event` may legally reference — mirrors every
/// variant `AutopilotEngine::AutopilotEvent::event_name()` can produce,
/// **except** `cep_trigger` itself (the synthetic trigger this module emits
/// is deliberately excluded so a sequence rule can never match on its own
/// output and form a feedback loop).
pub const KNOWN_EVENT_NAMES: &[&str] = &[
    "task_created",
    "task_updated",
    "task_status_changed",
    "activity_new",
    "channel_message",
    "agent_idle",
    "cron_tick",
    "run_at_risk",
    "os_file",
    "os_frontmost",
];

/// Bounds on `within_secs` — floor rejects a degenerate always-false-window
/// (`0`), ceiling caps a single pending entry's memory lifetime at 24h so a
/// forgotten sequence rule can't accumulate unbounded state age (the
/// per-rule pending-count cap in [`CepMatcher`] bounds count; this bounds
/// duration).
const WITHIN_SECS_MIN: u64 = 1;
const WITHIN_SECS_MAX: u64 = 86_400;

/// Validate a `sequence` rule-JSON value structurally at write time —
/// unknown event names, illegal operators, or an out-of-range `within_secs`
/// are all rejected here rather than discovered silently the first time a
/// matching event would have exercised the rule.
pub fn validate_sequence_spec(v: &Value) -> Result<(), String> {
    let spec: SequenceSpec =
        serde_json::from_value(v.clone()).map_err(|e| format!("invalid sequence: {e}"))?;
    validate_step(&spec.first, "first")?;
    validate_step(&spec.then, "then")?;
    if !(WITHIN_SECS_MIN..=WITHIN_SECS_MAX).contains(&spec.within_secs) {
        return Err(format!(
            "sequence.within_secs must be between {WITHIN_SECS_MIN} and {WITHIN_SECS_MAX} \
             (got {})",
            spec.within_secs
        ));
    }
    Ok(())
}

fn validate_step(step: &SequenceStep, label: &str) -> Result<(), String> {
    if !KNOWN_EVENT_NAMES.contains(&step.event.as_str()) {
        return Err(format!(
            "sequence.{label}.event unknown '{}'; must be one of: {}",
            step.event,
            KNOWN_EVENT_NAMES.join(", ")
        ));
    }
    validate_match_shape(&step.match_cond, label)
}

/// Structurally validate a condition tree (`all`/`any`/`{field,op,value}` —
/// same shape `autopilot_engine::evaluate` interprets at match time). `null`
/// is always valid ("no filter"). This does not evaluate anything — it only
/// rejects a condition tree that could never match legally (unknown op,
/// missing `field`, malformed nesting).
fn validate_match_shape(cond: &Value, label: &str) -> Result<(), String> {
    if cond.is_null() {
        return Ok(());
    }
    let obj = cond
        .as_object()
        .ok_or_else(|| format!("sequence.{label}.match must be a JSON object or null"))?;
    if let Some(all) = obj.get("all") {
        let arr = all
            .as_array()
            .ok_or_else(|| format!("sequence.{label}.match.all must be an array"))?;
        for c in arr {
            validate_match_shape(c, label)?;
        }
        return Ok(());
    }
    if let Some(any) = obj.get("any") {
        let arr = any
            .as_array()
            .ok_or_else(|| format!("sequence.{label}.match.any must be an array"))?;
        for c in arr {
            validate_match_shape(c, label)?;
        }
        return Ok(());
    }
    let field = obj
        .get("field")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("sequence.{label}.match.field is required"))?;
    let op = obj
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("sequence.{label}.match.op is required (field '{field}')"))?;
    if !CONDITION_OPS.contains(&op) {
        return Err(format!(
            "sequence.{label}.match.op unknown '{op}' (field '{field}'); must be one of: {}",
            CONDITION_OPS.join(", ")
        ));
    }
    if !obj.contains_key("value") {
        return Err(format!(
            "sequence.{label}.match.value is required (field '{field}')"
        ));
    }
    Ok(())
}

// ─── Matcher engine ─────────────────────────────────────────────────────

/// Max pending (opened-but-unresolved) windows kept per rule. Bounds memory
/// against a rule whose `first` fires far more often than `then` ever
/// resolves (or a negate rule that never sees `then` and would otherwise
/// grow unbounded until each entry's own deadline). Enforced with an
/// explicit drop-oldest + log — never a silent cap (opus-playbook §5: no
/// silent failure).
const MAX_PENDING_PER_RULE: usize = 100;

/// How often the background task scans for expired `negate` windows (the
/// "B never showed up in time" case, which nothing else observes — a
/// negative outcome has no triggering event of its own).
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// One open "saw `first`, waiting for `then`" window.
#[derive(Debug, Clone)]
struct PendingMatch {
    deadline: Instant,
    /// Flattened fields captured from the `first` event — used to render
    /// action templates on a `negate` timeout (there is no `then` event in
    /// that case) and merged under a `then` match too (see
    /// [`merge_trigger_fields`]).
    first_fields: serde_json::Map<String, Value>,
}

/// A resolved sequence pattern, ready to be turned into a
/// [`AutopilotEvent::CepTrigger`] and re-broadcast.
struct Resolution {
    rule_id: String,
    then_event: String,
    fields: serde_json::Map<String, Value>,
}

/// Merge `first` and (optional) `then` field maps into the map a
/// `CepTrigger`'s action templates render against. `then` wins on key
/// collision (it's the more proximate/relevant event); on a `negate`
/// timeout there is no `then` map and `first`'s fields are used as-is.
fn merge_trigger_fields(
    first: &serde_json::Map<String, Value>,
    then: Option<&serde_json::Map<String, Value>>,
) -> serde_json::Map<String, Value> {
    let mut out = first.clone();
    if let Some(then) = then {
        for (k, v) in then {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

pub struct CepMatcher {
    store: Arc<AutopilotStore>,
    /// rule_id -> FIFO queue of open windows. `VecDeque` so the
    /// drop-oldest-on-overflow and consume-earliest-on-match operations are
    /// both O(1) at the ends.
    pending: Mutex<HashMap<String, VecDeque<PendingMatch>>>,
}

impl CepMatcher {
    pub fn new(store: Arc<AutopilotStore>) -> Self {
        Self {
            store,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn the matcher's background task: consumes the autopilot broadcast
    /// bus (`rx`), re-broadcasts resolved patterns as
    /// [`AutopilotEvent::CepTrigger`] onto `tx` (the *same* bus —
    /// `AutopilotEngine::process_event` already subscribes to it), and runs
    /// the 30s expiry tick for `negate` timeouts.
    pub fn spawn(
        store: Arc<AutopilotStore>,
        mut rx: broadcast::Receiver<AutopilotEvent>,
        tx: broadcast::Sender<AutopilotEvent>,
    ) -> JoinHandle<()> {
        let matcher = Arc::new(Self::new(store));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TICK_INTERVAL);
            // Burst-safe: a delayed tick coalesces into one instead of firing
            // a backlog of ticks back-to-back (standard tokio pattern; the
            // other 30s/60s tickers in `server.rs` don't set this because
            // their bodies are cheap idempotent scans too, matching here).
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    recv = rx.recv() => {
                        match recv {
                            Ok(event) => {
                                let resolutions = matcher.on_event(&event).await;
                                for r in resolutions {
                                    let _ = tx.send(AutopilotEvent::CepTrigger {
                                        rule_id: r.rule_id,
                                        then_event: r.then_event,
                                        fields: Value::Object(r.fields),
                                    });
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                // A dropped event may mean a `first` or `then` was
                                // missed — visible in logs since (unlike
                                // AutopilotEngine's own lag handler) there's no
                                // dashboard Activity Feed row for "CEP window
                                // possibly desynced".
                                warn!(
                                    dropped_events = n,
                                    "cep_matcher: event bus lagged — {n} events dropped, \
                                     pending sequence windows may have missed a match"
                                );
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                debug!("cep_matcher: event bus closed, stopping");
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        let resolutions = matcher.expire_negate_windows().await;
                        for r in resolutions {
                            let _ = tx.send(AutopilotEvent::CepTrigger {
                                rule_id: r.rule_id,
                                then_event: r.then_event,
                                fields: Value::Object(r.fields),
                            });
                        }
                    }
                }
            }
        })
    }

    /// Process one incoming event against every enabled sequence rule.
    /// Returns any resolved (positive-match) patterns to re-broadcast.
    /// `negate` cancellations (B showed up in time) resolve here too but
    /// produce no [`Resolution`] — they just remove the pending window.
    async fn on_event(&self, event: &AutopilotEvent) -> Vec<Resolution> {
        // Never match against our own synthetic output — `cep_trigger` is
        // excluded from KNOWN_EVENT_NAMES so this is also enforced at
        // write-validation time, but the runtime check is cheap defense in
        // depth against a hand-crafted / legacy DB row.
        let event_name = event.event_name();
        if event_name == "cep_trigger" {
            return Vec::new();
        }
        let fields = event.to_fields();

        let rules = match self.store.list_rules().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "cep_matcher: list_rules failed, skipping this event");
                return Vec::new();
            }
        };

        let mut out = Vec::new();
        let now = Instant::now();
        for rule in rules.iter().filter(|r| r.enabled) {
            let Some(seq_json) = rule.sequence.as_deref() else {
                continue;
            };
            let spec: SequenceSpec = match serde_json::from_str(seq_json) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        rule_id = %rule.id, rule = %rule.name, error = %e,
                        "cep_matcher: rule has invalid sequence JSON (should have been \
                         rejected at write time) — skipping"
                    );
                    continue;
                }
            };

            let mut map = self.pending.lock().await;
            let queue = map.entry(rule.id.clone()).or_default();
            // Opportunistically drop windows whose deadline already passed —
            // keeps the queue length check below meaningful (an expired
            // entry doesn't count against the live cap) and avoids a
            // then-match consuming an already-timed-out window.
            queue.retain(|p| p.deadline > now);

            // ── `then` check: does this event complete (or, negated,
            // cancel) an open window? ──
            if event_name == spec.then.event && evaluate(&spec.then.match_cond, &fields) {
                if spec.negate {
                    // B arrived within the window → negation cancels, no fire.
                    queue.pop_front();
                } else if let Some(opened) = queue.pop_front() {
                    out.push(Resolution {
                        rule_id: rule.id.clone(),
                        then_event: event_name.to_string(),
                        fields: merge_trigger_fields(&opened.first_fields, Some(&fields)),
                    });
                }
            }

            // ── `first` check: does this event open a new window? ──
            // Checked independently of the `then` branch above — a single
            // event can legitimately serve as both a completion for an
            // older window and the start of a new one for the same rule.
            if event_name == spec.first.event && evaluate(&spec.first.match_cond, &fields) {
                if queue.len() >= MAX_PENDING_PER_RULE {
                    // No-silent-caps: an operator whose `first` fires much
                    // faster than `then` resolves needs to see this, not
                    // silently lose windows.
                    warn!(
                        rule_id = %rule.id, rule = %rule.name,
                        cap = MAX_PENDING_PER_RULE,
                        "cep_matcher: pending window cap hit — dropping oldest open window"
                    );
                    queue.pop_front();
                }
                queue.push_back(PendingMatch {
                    deadline: now + Duration::from_secs(spec.within_secs.max(1)),
                    first_fields: fields.clone(),
                });
            }
        }
        out
    }

    /// 30s tick: fire any `negate` rule whose window elapsed without ever
    /// seeing `then` (the "B did not show up in time" outcome — nothing
    /// else observes this since there is no event to hang it off).
    /// Non-negate windows that simply expire are dropped silently at the
    /// `debug` level — an elapsed positive window is the ordinary "A
    /// happened, B never followed" non-event, not an anomaly.
    async fn expire_negate_windows(&self) -> Vec<Resolution> {
        let rules = match self.store.list_rules().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "cep_matcher: list_rules failed during expiry tick");
                return Vec::new();
            }
        };

        let mut out = Vec::new();
        let now = Instant::now();
        let mut map = self.pending.lock().await;
        for rule in rules.iter().filter(|r| r.enabled) {
            let Some(seq_json) = rule.sequence.as_deref() else {
                continue;
            };
            let spec: SequenceSpec = match serde_json::from_str(seq_json) {
                Ok(s) => s,
                Err(_) => continue, // already warned in on_event
            };
            let Some(queue) = map.get_mut(&rule.id) else {
                continue;
            };
            if spec.negate {
                let mut remaining = VecDeque::with_capacity(queue.len());
                while let Some(p) = queue.pop_front() {
                    if p.deadline <= now {
                        out.push(Resolution {
                            rule_id: rule.id.clone(),
                            then_event: spec.then.event.clone(),
                            fields: merge_trigger_fields(&p.first_fields, None),
                        });
                    } else {
                        remaining.push_back(p);
                    }
                }
                *queue = remaining;
            } else {
                let before = queue.len();
                queue.retain(|p| p.deadline > now);
                let dropped = before - queue.len();
                if dropped > 0 {
                    debug!(
                        rule_id = %rule.id, rule = %rule.name, dropped,
                        "cep_matcher: positive-sequence window(s) expired without a `then` match"
                    );
                }
            }
        }
        out
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autopilot_store::AutopilotRuleRow;

    fn seq_rule(id: &str, sequence: Value) -> AutopilotRuleRow {
        AutopilotRuleRow {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            // Placeholder — sequence rules never dispatch via trigger_event
            // (see AutopilotEngine::process_event's `sequence.is_none()`
            // filter); any KNOWN value from the dashboard's existing
            // validator works here.
            trigger_event: "cron_tick".into(),
            conditions: "{}".into(),
            action: "{}".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: Some(sequence.to_string()),
            metadata: None,
        }
    }

    fn task_created(id: &str) -> AutopilotEvent {
        AutopilotEvent::TaskCreated {
            task: serde_json::json!({ "id": id, "title": "t" }),
        }
    }

    fn activity(kind: &str) -> AutopilotEvent {
        AutopilotEvent::ActivityNew {
            activity: serde_json::json!({ "kind": kind }),
        }
    }

    async fn store_with(rules: Vec<AutopilotRuleRow>) -> Arc<AutopilotStore> {
        let dir = std::env::temp_dir().join(format!("duduclaw-cep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(AutopilotStore::open(&dir).unwrap());
        for r in &rules {
            store.insert_rule(r).await.unwrap();
        }
        store
    }

    // ── Validation ───────────────────────────────────────────

    #[test]
    fn validate_accepts_well_formed_sequence() {
        let v = serde_json::json!({
            "first": { "event": "task_created", "match": null },
            "then": { "event": "activity_new", "match": {
                "field": "kind", "op": "eq", "value": "done"
            }},
            "within_secs": 600,
            "negate": false
        });
        assert!(validate_sequence_spec(&v).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_event_name() {
        let v = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "totally_made_up" },
            "within_secs": 60
        });
        let err = validate_sequence_spec(&v).unwrap_err();
        assert!(err.contains("unknown"), "{err}");
    }

    #[test]
    fn validate_rejects_cep_trigger_as_event_name() {
        // The synthetic output event must never be usable as a pattern input
        // — that would let a sequence rule match on its own trigger.
        let v = serde_json::json!({
            "first": { "event": "cep_trigger" },
            "then": { "event": "activity_new" },
            "within_secs": 60
        });
        assert!(validate_sequence_spec(&v).is_err());
    }

    #[test]
    fn validate_rejects_unknown_op() {
        let v = serde_json::json!({
            "first": { "event": "task_created", "match": {
                "field": "x", "op": "regex_match", "value": "1"
            }},
            "then": { "event": "activity_new" },
            "within_secs": 60
        });
        let err = validate_sequence_spec(&v).unwrap_err();
        assert!(err.contains("op unknown"), "{err}");
    }

    #[test]
    fn validate_rejects_out_of_range_within_secs() {
        let too_small = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 0
        });
        assert!(validate_sequence_spec(&too_small).is_err());

        let too_large = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 86_401
        });
        assert!(validate_sequence_spec(&too_large).is_err());
    }

    #[test]
    fn validate_rejects_malformed_match_missing_value() {
        let v = serde_json::json!({
            "first": { "event": "task_created", "match": { "field": "x", "op": "eq" } },
            "then": { "event": "activity_new" },
            "within_secs": 60
        });
        let err = validate_sequence_spec(&v).unwrap_err();
        assert!(err.contains("value is required"), "{err}");
    }

    #[test]
    fn validate_all_any_nested_shape() {
        let v = serde_json::json!({
            "first": { "event": "task_created", "match": {
                "all": [
                    { "field": "task.priority", "op": "eq", "value": "high" },
                    { "any": [
                        { "field": "task.status", "op": "eq", "value": "open" },
                        { "field": "task.status", "op": "eq", "value": "pending" }
                    ]}
                ]
            }},
            "then": { "event": "activity_new" },
            "within_secs": 300
        });
        assert!(validate_sequence_spec(&v).is_ok());
    }

    // ── Matching: positive (A then B within window) ────────────

    #[tokio::test(flavor = "current_thread")]
    async fn fires_when_then_arrives_within_window() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600,
            "negate": false
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        let out1 = matcher.on_event(&task_created("t1")).await;
        assert!(out1.is_empty(), "first alone must not fire yet");

        let out2 = matcher.on_event(&activity("anything")).await;
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].rule_id, "r1");
        assert_eq!(out2[0].then_event, "activity_new");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn does_not_fire_without_first() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        // `then` with no preceding `first` must not fire.
        let out = matcher.on_event(&activity("x")).await;
        assert!(out.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn does_not_fire_when_then_match_condition_fails() {
        // `ActivityNew.to_fields()` nests the payload under "activity", so
        // the dotted path exercises the same `lookup_path_opt` machinery
        // ordinary autopilot conditions use (autopilot_engine::evaluate is
        // reused as-is — no bespoke CEP condition language).
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new", "match": {
                "field": "activity.kind", "op": "eq", "value": "done"
            }},
            "within_secs": 600
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        matcher.on_event(&task_created("t1")).await;
        let out = matcher.on_event(&activity("wrong_kind")).await;
        assert!(out.is_empty());

        let out2 = matcher.on_event(&activity("done")).await;
        assert_eq!(out2.len(), 1);
    }

    // ── Matching: window expiry (outside window → no fire) ─────

    #[tokio::test(flavor = "current_thread")]
    async fn does_not_fire_when_then_arrives_after_window_closed() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            // Smallest legal window; we sleep past it below.
            "within_secs": 1
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        matcher.on_event(&task_created("t1")).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let out = matcher.on_event(&activity("x")).await;
        assert!(
            out.is_empty(),
            "then arriving after the window closed must not fire"
        );
    }

    // ── Matching: negate (A then NOT B within window) ───────────

    #[tokio::test(flavor = "current_thread")]
    async fn negate_fires_on_expiry_when_then_never_arrives() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 1,
            "negate": true
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        matcher.on_event(&task_created("t1")).await;
        // Immediate tick: window not yet expired → nothing fires.
        let too_early = matcher.expire_negate_windows().await;
        assert!(too_early.is_empty());

        tokio::time::sleep(Duration::from_millis(1100)).await;
        let out = matcher.expire_negate_windows().await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule_id, "r1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn negate_cancelled_when_then_arrives_in_time() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600,
            "negate": true
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        matcher.on_event(&task_created("t1")).await;
        // B arrives within the window → cancelled, no fire from the event...
        let on_event_out = matcher.on_event(&activity("anything")).await;
        assert!(on_event_out.is_empty(), "negate cancellation fires nothing");

        // ...and the expiry tick (even well past a would-be deadline) must
        // not fire either, because the window was already consumed.
        let out = matcher.expire_negate_windows().await;
        assert!(out.is_empty());
    }

    // ── Pending cap (no-silent-caps: oldest dropped, not silently lost) ──

    #[tokio::test(flavor = "current_thread")]
    async fn pending_cap_drops_oldest_not_silently() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        // Push well past the cap.
        for i in 0..(MAX_PENDING_PER_RULE + 10) {
            matcher.on_event(&task_created(&format!("t{i}"))).await;
        }
        let map = matcher.pending.lock().await;
        let queue = map.get("r1").expect("rule must have a pending queue");
        assert_eq!(
            queue.len(),
            MAX_PENDING_PER_RULE,
            "queue must be capped, not unbounded"
        );
    }

    // ── Own synthetic output never re-triggers matching ─────────

    #[tokio::test(flavor = "current_thread")]
    async fn cep_trigger_event_is_never_matched_against() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600
        });
        let store = store_with(vec![seq_rule("r1", spec)]).await;
        let matcher = CepMatcher::new(store);

        let synthetic = AutopilotEvent::CepTrigger {
            rule_id: "r1".into(),
            then_event: "activity_new".into(),
            fields: serde_json::json!({}),
        };
        let out = matcher.on_event(&synthetic).await;
        assert!(out.is_empty());
        // And it must not have been mistaken for a `first` match either.
        let map = matcher.pending.lock().await;
        assert!(map.get("r1").map(|q| q.is_empty()).unwrap_or(true));
    }

    // ── Disabled rules are inert ─────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn disabled_rule_never_opens_a_window() {
        let spec = serde_json::json!({
            "first": { "event": "task_created" },
            "then": { "event": "activity_new" },
            "within_secs": 600
        });
        let mut rule = seq_rule("r1", spec);
        rule.enabled = false;
        let store = store_with(vec![rule]).await;
        let matcher = CepMatcher::new(store);

        matcher.on_event(&task_created("t1")).await;
        let out = matcher.on_event(&activity("x")).await;
        assert!(out.is_empty());
    }

    // ── Field merge semantics ─────────────────────────────────────

    #[test]
    fn merge_then_wins_on_key_collision() {
        let mut first = serde_json::Map::new();
        first.insert("shared".into(), Value::String("from_first".into()));
        first.insert("only_first".into(), Value::String("f".into()));
        let mut then = serde_json::Map::new();
        then.insert("shared".into(), Value::String("from_then".into()));
        then.insert("only_then".into(), Value::String("t".into()));

        let merged = merge_trigger_fields(&first, Some(&then));
        assert_eq!(merged.get("shared").unwrap(), "from_then");
        assert_eq!(merged.get("only_first").unwrap(), "f");
        assert_eq!(merged.get("only_then").unwrap(), "t");
    }

    #[test]
    fn merge_negate_timeout_uses_first_only() {
        let mut first = serde_json::Map::new();
        first.insert("x".into(), Value::String("v".into()));
        let merged = merge_trigger_fields(&first, None);
        assert_eq!(merged.get("x").unwrap(), "v");
    }
}
