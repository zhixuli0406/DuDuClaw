//! OS ground-truth reconciliation (P3-3 of the runtime-agnostic security
//! redesign).
//!
//! # Why this exists
//!
//! An MCP tool call and an OS syscall are **not** 1:1. An agent subprocess can
//! touch the filesystem or the network without any tool call to explain it
//! (sandbox escape / hidden side effect), and it can emit a tool call that
//! *claims* an effect the OS never actually performed (false success,
//! arXiv:2606.09863). Reconciliation is therefore a pair of **deterministic set
//! differences** between two sides:
//!
//!   1. **unaccounted** — an observed OS event (a write *outside* the workspace
//!      roots, or an outbound network connection) with no compatible tool call
//!      inside the time window → high risk.
//!   2. **missing** — a tool call whose expected effect leaves an OS footprint
//!      (a write) yet no such footprint is observed inside the window → possible
//!      false success.
//!
//! # Layering
//!
//!   - The [`reconcile`] core is pure: zero I/O, deterministic, re-runnable. All
//!     time comparison is RFC3339 + a `±grace` window (mirroring the 2-second
//!     grace already used by `audit::read_tool_calls_since`).
//!   - Time-parse failures **fail safe toward reporting**, never toward passing:
//!     an observed event we cannot place in time is treated as unaccounted, and
//!     a claimed action we cannot place in time is treated as missing. We would
//!     rather raise a false alarm than let an escape slip through silently.
//!   - The [`ActionObserver`] trait abstracts the OS event source. [`ReplayObserver`]
//!     feeds recorded fixtures so the whole pipeline is CI-testable without any
//!     privilege. Real observers (macOS `eslogger`, Linux eBPF) live in the
//!     platform submodules and degrade gracefully when they lack permission.
//!   - [`escalation_floor_from_report`] maps a report onto a P3-2
//!     [`SecurityPosture`] floor, closing the loop: **P3-3 is a signal source for
//!     P3-2**.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::security_posture::{EscalationFloor, SecurityPosture};

// ── Observed OS events ────────────────────────────────────────

/// The class of an OS-level event observed for an agent subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsEventKind {
    /// The process wrote to a file.
    FileWrite,
    /// The process read a file.
    FileRead,
    /// The process opened an outbound network connection.
    NetConnect,
    /// The process executed another program.
    ProcExec,
}

/// A single OS ground-truth event attributed to an agent subprocess (by pid).
///
/// `path_or_endpoint` is a filesystem path for `FileWrite`/`FileRead`/`ProcExec`
/// and a host/endpoint string for `NetConnect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsEvent {
    pub kind: OsEventKind,
    pub path_or_endpoint: String,
    pub pid: u32,
    /// RFC3339 timestamp of the event.
    pub ts: String,
}

// ── Claimed actions (from tool_calls.jsonl) ───────────────────

/// The OS footprint a tool call is expected to leave, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedEffect {
    /// The tool is expected to write to a file.
    FileWrite,
    /// The tool is expected to open an outbound connection.
    NetConnect,
    /// The tool leaves no directly observable OS footprint (e.g. a pure query).
    None,
}

impl ExpectedEffect {
    /// Whether this effect leaves an OS footprint we can reconcile against.
    /// `None` cannot go "missing" — there is nothing to observe.
    pub fn has_footprint(self) -> bool {
        matches!(self, ExpectedEffect::FileWrite | ExpectedEffect::NetConnect)
    }

    /// Whether an observed event of `kind` satisfies this expected effect.
    fn matches_kind(self, kind: OsEventKind) -> bool {
        match self {
            ExpectedEffect::FileWrite => kind == OsEventKind::FileWrite,
            ExpectedEffect::NetConnect => kind == OsEventKind::NetConnect,
            ExpectedEffect::None => false,
        }
    }
}

/// Explicit set of write-class tools (initial table; extend as the tool surface
/// grows). Kept separate from the token heuristic below so a rename that drops
/// the literal `write` token still classifies correctly.
const WRITE_TOOLS: &[&str] = &[
    "shared_wiki_write",
    "shared_wiki_delete",
    "agent_update_soul",
    "create_agent",
];

/// Map a tool name to its expected OS effect (initial "tool → effect" table).
///
/// Rules (deterministic, order matters):
///   - `send_*` / `web_*` → [`ExpectedEffect::NetConnect`].
///   - a name whose underscore-delimited tokens include `write`, or that is in
///     [`WRITE_TOOLS`] → [`ExpectedEffect::FileWrite`].
///   - everything else → [`ExpectedEffect::None`].
pub fn expected_effect_for_tool(tool_name: &str) -> ExpectedEffect {
    // Net-effecting tools: exact ASCII prefixes on a controlled tool name (not
    // an allowlist over untrusted input), so prefix matching is safe here.
    if tool_name.starts_with("send_") || tool_name.starts_with("web_") {
        return ExpectedEffect::NetConnect;
    }
    if WRITE_TOOLS.contains(&tool_name)
        || tool_name.split('_').any(|token| token == "write")
    {
        return ExpectedEffect::FileWrite;
    }
    ExpectedEffect::None
}

/// A tool call parsed into the reconciliation domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimedAction {
    pub tool_name: String,
    /// RFC3339 timestamp the tool call was recorded at.
    pub ts: String,
    pub expected: ExpectedEffect,
}

impl ClaimedAction {
    /// Parse one `tool_calls.jsonl` record (as produced by
    /// `audit::append_tool_call*`) into a [`ClaimedAction`].
    ///
    /// Only successful calls are considered: a failed tool call makes no claim
    /// of an OS effect, so it can neither go missing nor explain a footprint.
    /// Returns `None` when the record is missing required fields or did not
    /// succeed.
    pub fn from_tool_call(record: &serde_json::Value) -> Option<ClaimedAction> {
        let success = record.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        if !success {
            return None;
        }
        let tool_name = record.get("tool_name").and_then(|v| v.as_str())?.to_string();
        let ts = record.get("timestamp").and_then(|v| v.as_str())?.to_string();
        let expected = expected_effect_for_tool(&tool_name);
        Some(ClaimedAction { tool_name, ts, expected })
    }
}

// ── Reconciliation report ─────────────────────────────────────

/// The deterministic result of reconciling claimed actions against observed OS
/// events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Number of observed relevant events matched to a compatible claim.
    pub matched: usize,
    /// Observed OS events with no tool call to explain them (high risk).
    pub unaccounted: Vec<OsEvent>,
    /// Tool calls that claimed a footprint-leaving effect but left no footprint.
    pub missing: Vec<ClaimedAction>,
}

impl ReconcileReport {
    /// A clean report — nothing to reconcile.
    pub fn clean() -> Self {
        Self { matched: 0, unaccounted: Vec::new(), missing: Vec::new() }
    }

    /// Whether the report is fully clean (no discrepancies).
    pub fn is_clean(&self) -> bool {
        self.unaccounted.is_empty() && self.missing.is_empty()
    }
}

/// Parse an RFC3339 timestamp into a UTC instant.
fn parse_ts(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Whether two instants fall within `±grace` seconds of each other.
fn within_grace(
    a: chrono::DateTime<chrono::Utc>,
    b: chrono::DateTime<chrono::Utc>,
    grace_secs: i64,
) -> bool {
    (a - b).num_seconds().abs() <= grace_secs
}

/// Whether a file path lives under any of the workspace roots.
///
/// Uses component-based [`Path::starts_with`] (not substring matching), so a
/// sibling directory sharing a name prefix cannot be mistaken for a child.
fn is_inside_workspace(path: &str, workspace_roots: &[PathBuf]) -> bool {
    let p = Path::new(path);
    workspace_roots.iter().any(|root| p.starts_with(root))
}

/// An observed event is *relevant* to the unaccounted-side check when it is a
/// write outside every workspace root, or any outbound network connection.
fn is_relevant_for_unaccounted(ev: &OsEvent, workspace_roots: &[PathBuf]) -> bool {
    match ev.kind {
        OsEventKind::FileWrite => !is_inside_workspace(&ev.path_or_endpoint, workspace_roots),
        OsEventKind::NetConnect => true,
        OsEventKind::FileRead | OsEventKind::ProcExec => false,
    }
}

/// Pure deterministic reconciliation core (zero I/O).
///
/// - **unaccounted**: every relevant observed event (write-outside-workspace or
///   net-connect) with no compatible claimed action inside the `±grace` window.
/// - **missing**: every claimed action whose expected effect leaves a footprint,
///   yet no observed event of the matching category exists inside the window.
///
/// Fail-safe: an observed event whose `ts` cannot be parsed is counted as
/// unaccounted (we cannot prove it was explained); a claimed action whose `ts`
/// cannot be parsed is counted as missing (we cannot prove its footprint).
pub fn reconcile(
    claimed: &[ClaimedAction],
    observed: &[OsEvent],
    workspace_roots: &[PathBuf],
    grace_secs: i64,
) -> ReconcileReport {
    let mut matched = 0usize;
    let mut unaccounted = Vec::new();
    let mut missing = Vec::new();

    // ── unaccounted: observed → claimed ──
    for ev in observed {
        if !is_relevant_for_unaccounted(ev, workspace_roots) {
            continue;
        }
        let ev_ts = match parse_ts(&ev.ts) {
            Some(t) => t,
            None => {
                // Fail safe: an event we cannot place in time is unexplained.
                unaccounted.push(ev.clone());
                continue;
            }
        };
        let explained = claimed.iter().any(|c| {
            if !c.expected.matches_kind(ev.kind) {
                return false;
            }
            match parse_ts(&c.ts) {
                Some(c_ts) => within_grace(ev_ts, c_ts, grace_secs),
                None => false, // an unparseable claim cannot explain anything
            }
        });
        if explained {
            matched += 1;
        } else {
            unaccounted.push(ev.clone());
        }
    }

    // ── missing: claimed → observed ──
    for c in claimed {
        if !c.expected.has_footprint() {
            continue;
        }
        let c_ts = match parse_ts(&c.ts) {
            Some(t) => t,
            None => {
                // Fail safe: a claim we cannot place in time is unverifiable.
                missing.push(c.clone());
                continue;
            }
        };
        // A footprint anywhere (in or out of the workspace) satisfies the claim.
        let has_footprint = observed.iter().any(|ev| {
            if !c.expected.matches_kind(ev.kind) {
                return false;
            }
            match parse_ts(&ev.ts) {
                Some(ev_ts) => within_grace(ev_ts, c_ts, grace_secs),
                None => false,
            }
        });
        if !has_footprint {
            missing.push(c.clone());
        }
    }

    ReconcileReport { matched, unaccounted, missing }
}

/// Map a reconciliation report onto a P3-2 [`SecurityPosture`] escalation floor.
///
/// - any unaccounted event → [`SecurityPosture::Red`] (possible escape).
/// - only missing claims → [`SecurityPosture::Yellow`] (possible false success).
/// - otherwise → [`SecurityPosture::Green`].
pub fn escalation_floor_from_report(report: &ReconcileReport) -> EscalationFloor {
    if !report.unaccounted.is_empty() {
        SecurityPosture::Red
    } else if !report.missing.is_empty() {
        SecurityPosture::Yellow
    } else {
        SecurityPosture::Green
    }
}

// ── Observer abstraction ──────────────────────────────────────

/// Availability of a concrete [`ActionObserver`] on the current host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// The observer is fully live and enforcing.
    Enforcing,
    /// The observer exists on this platform but cannot capture right now
    /// (missing privilege / not wired). Reconciliation degrades to a
    /// tool-calls-only self-consistency check. The string explains why.
    Degraded(String),
    /// The observer is not supported on this platform at all.
    Unsupported,
}

/// A source of OS ground-truth events for a given subprocess pid.
pub trait ActionObserver {
    /// Collect OS events for `pid` at or after the RFC3339 `since` timestamp.
    fn collect_since(&self, pid: u32, since: &str) -> Result<Vec<OsEvent>, String>;
    /// The observer's current availability on this host.
    fn availability(&self) -> Availability;
}

/// A deterministic observer backed by recorded fixtures. Makes the whole
/// pipeline testable in CI without any privilege.
pub struct ReplayObserver {
    events: Vec<OsEvent>,
}

impl ReplayObserver {
    pub fn new(events: Vec<OsEvent>) -> Self {
        Self { events }
    }
}

impl ActionObserver for ReplayObserver {
    fn collect_since(&self, pid: u32, since: &str) -> Result<Vec<OsEvent>, String> {
        // Fail-safe on an unparseable `since`: return everything for the pid
        // (over-reporting is preferable to dropping events silently).
        let since_dt = parse_ts(since);
        Ok(self
            .events
            .iter()
            .filter(|ev| ev.pid == pid)
            .filter(|ev| match (since_dt, parse_ts(&ev.ts)) {
                (Some(s), Some(t)) => t >= s,
                _ => true,
            })
            .cloned()
            .collect())
    }

    fn availability(&self) -> Availability {
        Availability::Enforcing
    }
}

// ── Platform observers ────────────────────────────────────────

pub mod ebpf;
pub mod eslogger;

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: OsEventKind, path: &str, pid: u32, ts: &str) -> OsEvent {
        OsEvent { kind, path_or_endpoint: path.into(), pid, ts: ts.into() }
    }

    fn write_claim(tool: &str, ts: &str) -> ClaimedAction {
        ClaimedAction { tool_name: tool.into(), ts: ts.into(), expected: ExpectedEffect::FileWrite }
    }

    // ── unaccounted set difference ───────────────────────────

    #[test]
    fn write_outside_workspace_with_no_claim_is_unaccounted() {
        let roots = vec![PathBuf::from("/work/agent-a")];
        let observed = vec![ev(OsEventKind::FileWrite, "/etc/evil.conf", 42, "2026-07-06T12:00:00Z")];
        let report = reconcile(&[], &observed, &roots, 2);
        assert_eq!(report.unaccounted.len(), 1);
        assert_eq!(report.unaccounted[0].path_or_endpoint, "/etc/evil.conf");
        assert_eq!(report.matched, 0);
    }

    #[test]
    fn write_inside_workspace_is_not_unaccounted() {
        let roots = vec![PathBuf::from("/work/agent-a")];
        let observed =
            vec![ev(OsEventKind::FileWrite, "/work/agent-a/notes.md", 42, "2026-07-06T12:00:00Z")];
        let report = reconcile(&[], &observed, &roots, 2);
        assert!(report.is_clean(), "in-workspace write must not be flagged");
    }

    #[test]
    fn net_connect_is_always_relevant_and_needs_a_claim() {
        let observed =
            vec![ev(OsEventKind::NetConnect, "evil.example.com:443", 42, "2026-07-06T12:00:00Z")];
        let report = reconcile(&[], &observed, &[], 2);
        assert_eq!(report.unaccounted.len(), 1);
    }

    #[test]
    fn explained_outside_write_is_matched_not_unaccounted() {
        let observed = vec![ev(OsEventKind::FileWrite, "/tmp/out.txt", 42, "2026-07-06T12:00:01Z")];
        let claimed = vec![write_claim("shared_wiki_write", "2026-07-06T12:00:00Z")];
        let report = reconcile(&claimed, &observed, &[], 2);
        assert!(report.unaccounted.is_empty());
        assert_eq!(report.matched, 1);
    }

    // ── missing set difference ───────────────────────────────

    #[test]
    fn write_tool_with_no_footprint_is_missing() {
        let claimed = vec![write_claim("agent_update_soul", "2026-07-06T12:00:00Z")];
        let report = reconcile(&claimed, &[], &[], 2);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].tool_name, "agent_update_soul");
    }

    #[test]
    fn none_effect_tool_never_goes_missing() {
        let claimed = vec![ClaimedAction {
            tool_name: "memory_search".into(),
            ts: "2026-07-06T12:00:00Z".into(),
            expected: ExpectedEffect::None,
        }];
        let report = reconcile(&claimed, &[], &[], 2);
        assert!(report.is_clean());
    }

    #[test]
    fn write_tool_with_in_workspace_footprint_is_satisfied() {
        let roots = vec![PathBuf::from("/work/a")];
        let claimed = vec![write_claim("shared_wiki_write", "2026-07-06T12:00:00Z")];
        let observed = vec![ev(OsEventKind::FileWrite, "/work/a/wiki.md", 7, "2026-07-06T12:00:00Z")];
        let report = reconcile(&claimed, &observed, &roots, 2);
        assert!(report.missing.is_empty(), "in-workspace write satisfies the claim");
    }

    // ── grace window boundary ────────────────────────────────

    #[test]
    fn footprint_just_inside_grace_matches() {
        // 2s apart, grace = 2 → within.
        let claimed = vec![write_claim("shared_wiki_write", "2026-07-06T12:00:00Z")];
        let observed = vec![ev(OsEventKind::FileWrite, "/tmp/x", 7, "2026-07-06T12:00:02Z")];
        let report = reconcile(&claimed, &observed, &[], 2);
        assert!(report.missing.is_empty());
        assert_eq!(report.matched, 1);
    }

    #[test]
    fn footprint_just_outside_grace_is_both_missing_and_unaccounted() {
        // 3s apart, grace = 2 → out of window. The claim goes missing and the
        // orphaned write goes unaccounted.
        let claimed = vec![write_claim("shared_wiki_write", "2026-07-06T12:00:00Z")];
        let observed = vec![ev(OsEventKind::FileWrite, "/tmp/x", 7, "2026-07-06T12:00:03Z")];
        let report = reconcile(&claimed, &observed, &[], 2);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.unaccounted.len(), 1);
        assert_eq!(report.matched, 0);
    }

    // ── fail-safe on unparseable time ────────────────────────

    #[test]
    fn unparseable_observed_ts_is_unaccounted() {
        let observed = vec![ev(OsEventKind::FileWrite, "/tmp/x", 7, "not-a-timestamp")];
        let report = reconcile(&[], &observed, &[], 2);
        assert_eq!(report.unaccounted.len(), 1, "fail safe toward reporting");
    }

    #[test]
    fn unparseable_claim_ts_is_missing() {
        let claimed = vec![write_claim("shared_wiki_write", "garbage")];
        let report = reconcile(&claimed, &[], &[], 2);
        assert_eq!(report.missing.len(), 1);
    }

    // ── tool → effect table ──────────────────────────────────

    #[test]
    fn tool_effect_table() {
        assert_eq!(expected_effect_for_tool("shared_wiki_write"), ExpectedEffect::FileWrite);
        assert_eq!(expected_effect_for_tool("agent_update_soul"), ExpectedEffect::FileWrite);
        assert_eq!(expected_effect_for_tool("send_to_agent"), ExpectedEffect::NetConnect);
        assert_eq!(expected_effect_for_tool("web_fetch"), ExpectedEffect::NetConnect);
        assert_eq!(expected_effect_for_tool("memory_search"), ExpectedEffect::None);
        assert_eq!(expected_effect_for_tool("tasks_list"), ExpectedEffect::None);
    }

    // ── ClaimedAction::from_tool_call parsing ────────────────

    #[test]
    fn from_tool_call_parses_successful_write() {
        let record = serde_json::json!({
            "timestamp": "2026-07-06T12:00:00Z",
            "agent_id": "agnes",
            "tool_name": "shared_wiki_write",
            "params_summary": "path=policies/x.md",
            "success": true,
        });
        let claim = ClaimedAction::from_tool_call(&record).unwrap();
        assert_eq!(claim.tool_name, "shared_wiki_write");
        assert_eq!(claim.expected, ExpectedEffect::FileWrite);
    }

    #[test]
    fn from_tool_call_skips_failed_calls() {
        let record = serde_json::json!({
            "timestamp": "2026-07-06T12:00:00Z",
            "tool_name": "shared_wiki_write",
            "success": false,
        });
        assert!(ClaimedAction::from_tool_call(&record).is_none());
    }

    #[test]
    fn from_tool_call_requires_fields() {
        let record = serde_json::json!({ "success": true });
        assert!(ClaimedAction::from_tool_call(&record).is_none());
    }

    // ── ReplayObserver fixture filtering ─────────────────────

    #[test]
    fn replay_observer_filters_by_pid_and_since() {
        let obs = ReplayObserver::new(vec![
            ev(OsEventKind::FileWrite, "/tmp/a", 1, "2026-07-06T12:00:00Z"),
            ev(OsEventKind::FileWrite, "/tmp/b", 2, "2026-07-06T12:00:00Z"),
            ev(OsEventKind::FileWrite, "/tmp/c", 1, "2026-07-06T11:00:00Z"),
        ]);
        let got = obs.collect_since(1, "2026-07-06T11:30:00Z").unwrap();
        assert_eq!(got.len(), 1, "pid=1 and after since");
        assert_eq!(got[0].path_or_endpoint, "/tmp/a");
        assert_eq!(obs.availability(), Availability::Enforcing);
    }

    // ── report → escalation floor mapping ────────────────────

    #[test]
    fn floor_mapping() {
        let mut r = ReconcileReport::clean();
        assert_eq!(escalation_floor_from_report(&r), SecurityPosture::Green);
        r.missing.push(write_claim("shared_wiki_write", "2026-07-06T12:00:00Z"));
        assert_eq!(escalation_floor_from_report(&r), SecurityPosture::Yellow);
        r.unaccounted.push(ev(OsEventKind::NetConnect, "x:443", 1, "2026-07-06T12:00:00Z"));
        assert_eq!(escalation_floor_from_report(&r), SecurityPosture::Red);
    }
}
