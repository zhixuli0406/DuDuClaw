//! RM-027 `org_change`: a Manager's proposal to move an AI employee in the
//! org chart (`reports_to` / `department`), waiting for an Admin.
//!
//! Dashboard-only (`approval_notify::is_dashboard_only_kind`): channels get a
//! plain notice without buttons, and a channel decision is always refused.
//! The payload carries everything the decision needs — no sidecar:
//!
//! ```json
//! { "agent_id": "…",
//!   "before": { "reports_to": "…", "department": "…" },
//!   "after":  { "reports_to": "…", "department": "…" },
//!   "base": "<sha256 of the effective org entry when proposed>",
//!   "proposer": "<user id>", "proposer_email": "…", "reason": "…" }
//! ```
//!
//! On approve the gateway takes a short decision lease on the card (the TTL
//! sweep cannot expire it underneath), then the global org mutation lock,
//! re-reads the card (an approval that expired while waiting is not applied),
//! re-validates the move against the tree as it is *now*, refuses (and
//! denies) a request whose `base` no longer matches, writes a *prepared*
//! [`journal`] entry, applies — authority first, mirror second — marks the
//! entry *applied*, and records the decision under the lease only after the
//! write succeeded. If recording the decision fails, the journal lets a
//! retry complete the original approval instead of judging the (now moved)
//! base as stale. Expiry counts as a denial. At most one request per AI
//! employee waits at a time.

use serde_json::{Value, json};

use crate::approval::{ApprovalRecord, ApprovalStatus};
use duduclaw_core::org_store::OrgEntry;

pub(crate) const ACTION_KIND: &str = "org_change";
/// Three days; expiry counts as a denial.
pub(crate) const TTL_SECS: i64 = 3 * 24 * 3600;
/// Shown when an expired request is decided.
pub(crate) const EXPIRED_TEXT: &str =
    "這個組織調整的審核已逾期，調整沒有套用；需要的話請重新提出。";
/// Decision lease (seconds) an approving Admin holds on the card while the
/// change lands: long enough for the org lock + authority + mirror write,
/// short enough that a crashed attempt frees the card quickly.
pub(crate) const DECISION_LEASE_SECS: i64 = 120;
/// RM-027 round 5: the longest an `agent.toml` mirror write made under the
/// global org lock waits for that file's lock (in-process mutex + advisory
/// lock, one budget). An ordinary write holds it for milliseconds; 30 s
/// absorbs a slow disk or a lifecycle-barrier drain (`ADMISSION_WAIT` is
/// also 30 s) yet stays well under [`DECISION_LEASE_SECS`] — so even an
/// unrenewed lease outlives one full wait, and the org lock is never pinned
/// longer than this by a stuck writer. Past it the authority stays committed,
/// the journal records "mirror pending" and the recovery finishes the mirror.
pub(crate) const MIRROR_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// RM-027 round 5: the maximum life of a decision lease after its holder
/// committed (counted from `mark_committed`). What is left at that point is
/// one bounded mirror write ([`MIRROR_LOCK_WAIT`]), a journal write, the
/// registry rescan and the decision row — seconds. 180 s (six mirror waits,
/// 1.5 leases) is far beyond that; a holder still not done is stuck, and its
/// lease lapses so the applied journal's recovery (which takes a lease of
/// its own) records the outcome instead of the card staying pending forever.
pub(crate) const COMMITTED_LEASE_MAX: std::time::Duration = std::time::Duration::from_secs(180);
/// RM-027 round 5: when a background attempt finishes a "mirror pending"
/// journal entry after the approval (any later approve click on the card,
/// and an Admin re-applying the placement, also finish it).
pub(crate) const MIRROR_RECOVERY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(1800),
];

/// [`MIRROR_LOCK_WAIT`] for `home` (tests shorten it per home).
pub(crate) fn mirror_lock_wait(home: &std::path::Path) -> std::time::Duration {
    #[cfg(test)]
    {
        if let Some(d) = mirror_wait_override::get(home) {
            return d;
        }
    }
    let _ = home;
    MIRROR_LOCK_WAIT
}

/// Test seam: a shorter mirror lock wait for one home.
#[cfg(test)]
pub(crate) mod mirror_wait_override {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    static WAITS: Mutex<BTreeMap<PathBuf, Duration>> = Mutex::new(BTreeMap::new());

    pub(crate) fn set(home: &Path, wait: Duration) {
        WAITS.lock().unwrap().insert(home.to_path_buf(), wait);
    }

    pub(crate) fn clear(home: &Path) {
        WAITS.lock().unwrap().remove(home);
    }

    pub(super) fn get(home: &Path) -> Option<Duration> {
        WAITS.lock().ok().and_then(|w| w.get(home).copied())
    }
}
/// Longest `reason` kept in the payload.
pub(crate) const MAX_REASON_CHARS: usize = 1000;

/// `{reports_to, department}` as stored in the payload and returned by RPCs.
pub(crate) fn entry_json(e: &OrgEntry) -> Value {
    json!({ "reports_to": e.reports_to, "department": e.department })
}

fn entry_from(v: Option<&Value>) -> Option<OrgEntry> {
    let v = v?;
    Some(OrgEntry::new(
        v.get("reports_to")?.as_str()?,
        v.get("department")?.as_str()?,
    ))
}

/// The parsed payload of one `org_change` request.
#[derive(Debug, Clone)]
pub(crate) struct Proposal {
    pub agent_id: String,
    pub before: OrgEntry,
    pub after: OrgEntry,
    pub base: String,
    pub proposer: String,
    pub proposer_email: String,
    pub reason: String,
}

impl Proposal {
    pub(crate) fn to_payload(&self) -> Value {
        json!({
            "agent_id": self.agent_id,
            "before": entry_json(&self.before),
            "after": entry_json(&self.after),
            "base": self.base,
            "proposer": self.proposer,
            "proposer_email": self.proposer_email,
            "reason": self.reason,
        })
    }

    /// `None` when `rec` is not a well-formed `org_change` request (a
    /// malformed payload is never applied).
    pub(crate) fn from_record(rec: &ApprovalRecord) -> Option<Self> {
        if rec.action_kind != ACTION_KIND {
            return None;
        }
        let p = &rec.payload;
        let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
        let agent_id = s("agent_id")?;
        // The broker's own agent column must agree with the payload: the
        // decide gate authorises against the column.
        if agent_id != rec.agent_id || !duduclaw_core::is_valid_agent_id(&agent_id) {
            return None;
        }
        Some(Self {
            agent_id,
            before: entry_from(p.get("before"))?,
            after: entry_from(p.get("after"))?,
            base: s("base")?,
            proposer: s("proposer").unwrap_or_default(),
            proposer_email: s("proposer_email").unwrap_or_default(),
            reason: s("reason").unwrap_or_default(),
        })
    }
}

/// Still waiting and not past its deadline.
pub(crate) fn live_pending(rec: &ApprovalRecord) -> bool {
    rec.status == ApprovalStatus::Pending && !rec.is_stale(chrono::Utc::now())
}

/// RFC 3339 deadline of `rec` (`null` when `created_at` does not parse).
pub(crate) fn expires_at(rec: &ApprovalRecord) -> Value {
    chrono::DateTime::parse_from_rfc3339(&rec.created_at)
        .ok()
        .map(|t| (t + chrono::Duration::seconds(rec.ttl_seconds)).to_rfc3339())
        .map_or(Value::Null, Value::String)
}

/// zh-TW one-line summary for the approval card.
pub(crate) fn summary(p: &Proposal) -> String {
    let label = |e: &OrgEntry| {
        let boss = if e.reports_to.is_empty() { "最上層".to_string() } else { format!("向「{}」回報", e.reports_to) };
        let dept = if e.department.is_empty() { "無部門".to_string() } else { format!("部門「{}」", e.department) };
        format!("{boss}、{dept}")
    };
    format!(
        "{who} 提議調整 AI 員工「{agent}」的組織位置：{before} → {after}。",
        who = duduclaw_core::truncate_chars(
            if p.proposer_email.is_empty() { p.proposer.as_str() } else { p.proposer_email.as_str() },
            80,
        ),
        agent = p.agent_id,
        before = label(&p.before),
        after = label(&p.after),
    )
}

/// The `org_change` approval journal, keyed by approval id
/// (`<home>/org_change_journal/<id>.json`).
///
/// The approval path applies first and records the decision second, so the
/// journal brackets the authority commit (RM-027 round 3):
///
/// 1. **prepared** — written *before* the `org.toml` commit, inside the org
///    lock and the decision lease, carrying the approval id and the digest of
///    the payload being applied. If it cannot be written, nothing is applied.
/// 2. **applied** — rewritten in the same org lock right after the commit.
///
/// A retry that finds an *applied* entry finishes the *original* decision
/// instead of re-judging the (now moved) base as stale. A *prepared* entry
/// means a previous attempt stopped somewhere around the commit; the retry
/// reconciles it against the authority under the org lock (see
/// `decide_org_change`). The entry is removed once the decision is recorded,
/// or when the commit itself failed.
///
/// RM-027 round 5 — **mirror pending**: when the `agent.toml` mirror write
/// did not land (its lock wait is bounded by [`MIRROR_LOCK_WAIT`]) the entry
/// outlives the recorded decision (`mirror_synced = false`,
/// `decision_recorded = true`). The recovery (a background retry, the next
/// approve click on the card) re-checks under the org lock that the
/// authority still holds `after` and finishes the mirror; the entry is
/// removed once the mirror follows (or a later change superseded it).
pub(crate) mod journal {
    use super::*;
    use std::path::{Path, PathBuf};

    const DIR: &str = "org_change_journal";

    /// Where the bracketed commit got to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum State {
        /// Written before the authority commit; the commit may or may not
        /// have happened.
        Prepared,
        /// The authority commit happened.
        Applied,
    }

    impl State {
        fn as_str(self) -> &'static str {
            match self {
                Self::Prepared => "prepared",
                Self::Applied => "applied",
            }
        }

        fn parse(s: &str) -> Option<Self> {
            match s {
                "prepared" => Some(Self::Prepared),
                "applied" => Some(Self::Applied),
                _ => None,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Applied {
        pub approval_id: String,
        /// [`crate::approval::payload_hash`] of the approval's payload at
        /// prepare time; a retry refuses an entry whose digest no longer
        /// matches the card.
        pub payload_digest: String,
        pub state: State,
        pub agent_id: String,
        pub before: OrgEntry,
        pub after: OrgEntry,
        /// User id of the Admin whose approval was applied.
        pub approved_by: String,
        pub prepared_at: String,
        /// Empty while [`State::Prepared`].
        pub applied_at: String,
        pub registry_reloaded: bool,
        pub mirror_synced: bool,
        /// RM-027 round 5: the approval was recorded (and audited) but the
        /// `agent.toml` mirror is still behind — the entry is kept as the
        /// "mirror pending" record the recovery finishes, and nothing about
        /// the decision is redone.
        pub decision_recorded: bool,
    }

    /// Digest of an approval's payload, as journaled.
    pub(crate) fn payload_digest(rec: &ApprovalRecord) -> String {
        crate::approval::payload_hash(&rec.payload)
    }

    impl Applied {
        /// A prepared entry for applying `after` to `agent_id` under `rec`.
        pub(crate) fn prepared(
            rec: &ApprovalRecord,
            agent_id: &str,
            before: &OrgEntry,
            after: &OrgEntry,
            approved_by: &str,
        ) -> Self {
            Self {
                approval_id: rec.id.as_str().to_string(),
                payload_digest: payload_digest(rec),
                state: State::Prepared,
                agent_id: agent_id.to_string(),
                before: before.clone(),
                after: after.clone(),
                approved_by: approved_by.to_string(),
                prepared_at: chrono::Utc::now().to_rfc3339(),
                applied_at: String::new(),
                registry_reloaded: false,
                mirror_synced: false,
                decision_recorded: false,
            }
        }

        /// Kept after the decision was recorded: the mirror is still pending.
        pub(crate) fn mirror_pending_after_decision(self) -> Self {
            Self { decision_recorded: true, ..self }
        }

        /// The same entry, marked applied.
        pub(crate) fn into_applied(self, registry_reloaded: bool, mirror_synced: bool) -> Self {
            Self {
                state: State::Applied,
                applied_at: chrono::Utc::now().to_rfc3339(),
                registry_reloaded,
                mirror_synced,
                ..self
            }
        }

        /// Does this entry belong to `rec` exactly as it reads now?
        pub(crate) fn matches(&self, rec: &ApprovalRecord) -> bool {
            self.approval_id == rec.id.as_str() && self.payload_digest == payload_digest(rec)
        }

        fn to_json(&self) -> Value {
            json!({
                "approval_id": self.approval_id,
                "payload_digest": self.payload_digest,
                "state": self.state.as_str(),
                "agent_id": self.agent_id,
                "before": entry_json(&self.before),
                "after": entry_json(&self.after),
                "approved_by": self.approved_by,
                "prepared_at": self.prepared_at,
                "applied_at": self.applied_at,
                "registry_reloaded": self.registry_reloaded,
                "mirror_synced": self.mirror_synced,
                "decision_recorded": self.decision_recorded,
            })
        }

        fn from_json(v: &Value) -> Option<Self> {
            let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
            Some(Self {
                approval_id: s("approval_id")?,
                payload_digest: s("payload_digest").filter(|d| !d.is_empty())?,
                state: State::parse(&s("state")?)?,
                agent_id: s("agent_id")?,
                before: entry_from(v.get("before"))?,
                after: entry_from(v.get("after"))?,
                approved_by: s("approved_by")?,
                prepared_at: s("prepared_at").unwrap_or_default(),
                applied_at: s("applied_at").unwrap_or_default(),
                registry_reloaded: v.get("registry_reloaded").and_then(Value::as_bool).unwrap_or(false),
                mirror_synced: v.get("mirror_synced").and_then(Value::as_bool).unwrap_or(false),
                decision_recorded: v.get("decision_recorded").and_then(Value::as_bool).unwrap_or(false),
            })
        }
    }

    /// Approval ids are UUIDs; anything else never becomes a path.
    fn path_for(home: &Path, approval_id: &str) -> Option<PathBuf> {
        let safe = !approval_id.is_empty()
            && approval_id.len() <= 64
            && approval_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        safe.then(|| home.join(DIR).join(format!("{approval_id}.json")))
    }

    /// Test seam: make [`record`] fail for one approval id in one state.
    #[cfg(test)]
    pub(crate) mod record_failpoint {
        use std::collections::BTreeSet;
        use std::sync::Mutex;

        static ARMED: Mutex<BTreeSet<(String, &'static str)>> = Mutex::new(BTreeSet::new());

        pub(crate) fn arm(id: &str, state: super::State) {
            ARMED.lock().unwrap().insert((id.to_string(), state.as_str()));
        }

        pub(crate) fn disarm(id: &str, state: super::State) {
            ARMED.lock().unwrap().remove(&(id.to_string(), state.as_str()));
        }

        pub(super) fn armed(id: &str, state: super::State) -> bool {
            ARMED.lock().map(|s| s.contains(&(id.to_string(), state.as_str()))).unwrap_or(false)
        }
    }

    /// Persist `a` in its current [`State`] (atomic temp + rename).
    pub(crate) fn record(home: &Path, a: &Applied) -> Result<(), String> {
        #[cfg(test)]
        {
            if record_failpoint::armed(&a.approval_id, a.state) {
                return Err("injected journal write failure".into());
            }
        }
        let path = path_for(home, &a.approval_id).ok_or_else(|| "invalid approval id".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&a.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }

    /// `Ok(None)` when nothing was journaled for this id; `Err` when an entry
    /// exists but cannot be read (the caller fails closed).
    pub(crate) fn read(home: &Path, approval_id: &str) -> Result<Option<Applied>, String> {
        let Some(path) = path_for(home, approval_id) else {
            return Ok(None);
        };
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let v: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        match Applied::from_json(&v) {
            Some(a) if a.approval_id == approval_id => Ok(Some(a)),
            _ => Err("malformed org_change journal entry".into()),
        }
    }

    /// Drop the entry once the decision is recorded.
    pub(crate) fn clear(home: &Path, approval_id: &str) {
        if let Some(path) = path_for(home, approval_id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Is something journaled (or unreadable) for this id?
    pub(crate) fn exists(home: &Path, approval_id: &str) -> bool {
        path_for(home, approval_id).is_some_and(|p| p.exists())
    }
}

/// The plain channel notice (no decision verb, no buttons).
pub(crate) fn notice_body(rec: &ApprovalRecord, reminder: bool, deadline: &str) -> String {
    let head = if reminder {
        "⏰ 有一則組織調整的審核快到期了，逾時會自動拒絕"
    } else {
        "📥 有一則組織調整等待審核"
    };
    format!(
        "{head}\n\
         AI 員工：{agent}\n\
         這類調整只能由管理員在儀表板的待辦清單決定，請開啟儀表板處理。\n\
         期限：{deadline}未審核將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(payload: Value, agent: &str) -> ApprovalRecord {
        ApprovalRecord {
            id: crate::approval::ApprovalId::new(),
            agent_id: agent.into(),
            action_kind: ACTION_KIND.into(),
            summary: String::new(),
            payload,
            status: ApprovalStatus::Pending,
            created_at: chrono::Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: TTL_SECS,
            notify_channel: None,
            notify_chat_id: None,
            reminded_at: None,
            simulation: None,
            request_kind: Default::default(),
            binding: None,
            answer: None,
            invalidated_reason: None,
            on_behalf_of: None,
        }
    }

    #[test]
    fn payload_round_trips_and_a_mismatched_agent_column_is_refused() {
        let p = Proposal {
            agent_id: "rep".into(),
            before: OrgEntry::new("lead", "業務"),
            after: OrgEntry::new("ceo", ""),
            base: "b".repeat(64),
            proposer: "mgr".into(),
            proposer_email: "mgr@example.invalid".into(),
            reason: "調整".into(),
        };
        let back = Proposal::from_record(&rec(p.to_payload(), "rep")).unwrap();
        assert_eq!((back.before, back.after), (p.before.clone(), p.after.clone()));
        assert!(Proposal::from_record(&rec(p.to_payload(), "someone-else")).is_none());
        assert!(Proposal::from_record(&rec(json!({"agent_id": "rep"}), "rep")).is_none());
        assert!(summary(&p).contains("rep"));
        assert!(expires_at(&rec(p.to_payload(), "rep")).is_string());
    }

    #[test]
    fn the_journal_round_trips_and_never_builds_a_path_from_a_bad_id() {
        let h = tempfile::tempdir().unwrap();
        let card = rec(json!({ "agent_id": "rep", "base": "b" }), "rep");
        let id = card.id.clone();
        let prepared = journal::Applied::prepared(
            &card,
            "rep",
            &OrgEntry::new("lead", "業務"),
            &OrgEntry::new("ceo", ""),
            "admin-1",
        );
        assert_eq!(prepared.state, journal::State::Prepared);
        assert!(prepared.applied_at.is_empty());
        assert!(prepared.matches(&card));
        assert_eq!(journal::read(h.path(), id.as_str()), Ok(None));
        journal::record(h.path(), &prepared).unwrap();
        assert!(journal::exists(h.path(), id.as_str()));
        assert_eq!(journal::read(h.path(), id.as_str()), Ok(Some(prepared.clone())));
        let a = prepared.into_applied(true, false);
        assert_eq!(a.state, journal::State::Applied);
        journal::record(h.path(), &a).unwrap();
        assert_eq!(journal::read(h.path(), id.as_str()), Ok(Some(a.clone())));
        // The digest pins the payload: a changed card no longer matches.
        let mut changed = card.clone();
        changed.payload = json!({ "agent_id": "rep", "base": "other" });
        assert!(!a.matches(&changed));
        journal::clear(h.path(), id.as_str());
        assert_eq!(journal::read(h.path(), id.as_str()), Ok(None));
        let bad = journal::Applied { approval_id: "../escape".into(), ..a.clone() };
        assert!(journal::record(h.path(), &bad).is_err());
        assert!(!h.path().join("escape.json").exists());
        // An entry that exists but does not parse fails closed.
        std::fs::create_dir_all(h.path().join("org_change_journal")).unwrap();
        std::fs::write(h.path().join("org_change_journal").join(format!("{}.json", id.as_str())), "{").unwrap();
        assert!(journal::read(h.path(), id.as_str()).is_err());
    }

    #[test]
    fn the_kind_is_dashboard_only_with_its_own_texts() {
        assert!(crate::approval_notify::is_dashboard_only_kind(ACTION_KIND));
        assert_eq!(crate::approval_notify::dashboard_only_expired_text(ACTION_KIND), EXPIRED_TEXT);
        let body = crate::approval_notify::dashboard_only_notice_body(&rec(json!({}), "rep"), false);
        assert!(body.contains("組織調整") && body.contains("rep"), "{body}");
        assert!(!body.contains("知識"), "must not be worded as a knowledge review: {body}");
    }
}
