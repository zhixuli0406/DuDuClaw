//! Source lineage for memory rows (P2-B: forget by source, no resurrection).
//!
//! Every write into `memories` / `key_facts` records where it came from in the
//! `memory_origins` table, in the same SQLite transaction as the row itself.
//! A derived row copies ("flattens") all of its parents' sources at write time,
//! so forgetting a source later is one indexed lookup, never a graph walk.
//!
//! Forgetting writes tombstones (`forgotten_sources`, `forgotten_memories`).
//! Every write path checks them inside its transaction before writing; a DB
//! trigger is the last line (it aborts any insert the Rust check missed).
//!
//! The types here are the contract the gateway and CLI producers use:
//! [`SourceRef`] is always built by the host (never by the model), and
//! [`Provenance`] is a required argument of every engine write API.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) mod db;
pub(crate) mod hooks;
pub(crate) mod links;

pub use hooks::{ApplyHookPoint, HookPoint};

/// Maximum number of lineage rows one memory row may carry (direct +
/// inherited + reaffirm). A write that would exceed it is refused with
/// [`FenceReason::LineageOverflow`].
pub const MAX_LINEAGE_SOURCES: usize = 512;

/// Maximum byte length of a `session` or `message` key.
pub const MAX_SOURCE_KEY_BYTES: usize = 512;

/// The `producer` of [`Provenance::test_only`]. Production code must never use
/// it (locked by `lineage::tests::test_only_provenance_is_not_used_in_production`).
pub const TEST_ONLY_PRODUCER: &str = "test_only";

/// Session prefix reserved for [`Provenance::System`] rows.
pub const SYSTEM_SESSION_PREFIX: &str = "system:";

/// Session prefix of an imported file ([`SourceRef::import_item`]).
pub const IMPORT_SESSION_PREFIX: &str = "import:";

/// The canonical form of an import source id (H-3 / M-9, N8): the resolved
/// absolute path when it names an existing file or directory (symlinks and
/// `..` resolved); anything else — a logical id such as
/// `claude-code-session:<uuid>`, or a path that no longer exists — is taken
/// as given, so it never depends on the working directory. The same file
/// re-imported from its path stays forgotten; a copy at another path is a
/// different source.
pub fn canonical_import_id(id: &str) -> String {
    let p = std::path::Path::new(id);
    if p.exists() {
        if let Ok(c) = std::fs::canonicalize(p) {
            return c.to_string_lossy().into_owned();
        }
    }
    id.to_string()
}

/// The session of everything imported from `source_id` (a canonical id):
/// `import:<first 16 bytes of sha256(source id)>`.
pub fn import_session(source_id: &str) -> String {
    let digest = Sha256::digest(source_id.as_bytes());
    format!("{IMPORT_SESSION_PREFIX}{}", hex(&digest[..16]))
}

/// Watermark time of a whole-source forget of an import: later than any
/// observation, so a re-import is blocked whenever it happens.
pub const FOREVER_TS: &str = "9999-12-31T23:59:59.999999Z";

/// Session value of the placeholder row written when a parent has no lineage.
pub const UNTRACKED_SESSION: &str = "untracked";

/// `source_kind` of the placeholder row for an untracked parent.
pub const UNTRACKED_PARENT_KIND: &str = "untracked_parent";

/// Format a timestamp the one way every lineage column and tombstone uses:
/// UTC, microseconds, `Z` suffix. String comparison of two values produced by
/// this function orders them in time; any other format would compare wrong.
pub fn format_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The tombstone match expression the fence uses (one SQL template shared by
/// the Rust check, the trigger and plans), for callers that move rows with
/// SQL themselves (the gateway's stray-DB merge). True when the source given
/// by the column expressions matches a row of `tombstone_table`.
pub fn tombstone_match_expr(
    tombstone_table: &str,
    agent: &str,
    session: &str,
    message: &str,
    seq: &str,
    observed: &str,
) -> String {
    db::tombstone_match_sql(tombstone_table, agent, session, message, seq, observed)
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Full sha256 hex of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Audit-safe digest of a source or tombstone: the first 16 bytes (32 hex
/// chars) of `sha256(agent|session|message|upto)`. Never contains the raw keys.
pub fn source_digest(agent_id: &str, session: &str, message: &str, upto: &str) -> String {
    let joined = format!("{agent_id}|{session}|{message}|{upto}");
    let full = Sha256::digest(joined.as_bytes());
    hex(&full[..16])
}

/// Where a source came from. Stored as `memory_origins.source_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// One channel message: `session` = channel session id,
    /// `message` = `m:<session_messages.id>`, `seq` = that id.
    ChannelMessage,
    /// One dispatch run: `session` = `"{request_type}:{agent}"`,
    /// `message` = `run:<hash of prompt and reply>`.
    DispatchRun,
    /// One MCP turn of a host-spawned employee: `session` = host session id,
    /// `message` = `turn:<turn id>`.
    McpTurn,
    /// A call from an external MCP client: `session` = `mcp:<client_id>`,
    /// `message` = `call:<uuid>`.
    McpExternal,
    /// One record of an imported file: `session` = `import:<path digest>`,
    /// `message` = `item:<index>`.
    ImportItem,
    /// One day of OS footprint: `session` = `footprint:<agent>`,
    /// `message` = `day:<YYYY-MM-DD>`.
    FootprintDay,
    /// Non-conversation content (only produced by [`Provenance::System`]).
    System,
    /// A marker, not a forgettable source: the dispatch run that made this
    /// write was handed an incomplete upstream turn identity by its bus
    /// message, so the upstream conversation is unknown. `session` =
    /// [`UNTRACKED_SESSION`], `message` = `upstream:run:<run key>`. Counted
    /// with the untracked rows in a plan; never listed or forgotten.
    UpstreamUnknown,
}

impl SourceKind {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChannelMessage => "channel_message",
            Self::DispatchRun => "dispatch_run",
            Self::McpTurn => "mcp_turn",
            Self::McpExternal => "mcp_external",
            Self::ImportItem => "import_item",
            Self::FootprintDay => "footprint_day",
            Self::System => "system",
            Self::UpstreamUnknown => UPSTREAM_UNKNOWN_KIND,
        }
    }
}

/// `source_kind` of the [`SourceKind::UpstreamUnknown`] marker row.
pub const UPSTREAM_UNKNOWN_KIND: &str = "upstream_unknown";

/// One host-generated source of a memory write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRef {
    pub kind: SourceKind,
    pub session: String,
    pub message: String,
    /// `session_messages.id` — channel messages only.
    pub seq: Option<i64>,
    pub observed_at: DateTime<Utc>,
    /// Host-computed sha256 hex of the source text, when known.
    pub content_hash: Option<String>,
}

impl SourceRef {
    /// A channel message (`message = m:<seq>`).
    pub fn channel_message(
        session: impl Into<String>,
        seq: i64,
        observed_at: DateTime<Utc>,
        content_hash: Option<String>,
    ) -> Self {
        Self {
            kind: SourceKind::ChannelMessage,
            session: session.into(),
            message: format!("m:{seq}"),
            seq: Some(seq),
            observed_at,
            content_hash,
        }
    }

    /// Any non-channel source; `message` is the full key (`run:…`, `turn:…`…).
    pub fn other(
        kind: SourceKind,
        session: impl Into<String>,
        message: impl Into<String>,
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            kind,
            session: session.into(),
            message: message.into(),
            seq: None,
            observed_at,
            content_hash: None,
        }
    }

    /// An imported record, content-addressed (H-3 / M-9):
    /// `session = import:<first 16 bytes of sha256(source id)>` (callers pass
    /// a canonical path), `message = item:<first 16 bytes of sha256(record)>`.
    /// A whole `import:` session is forgotten without regard to time.
    pub fn import_item(source_id: &str, record: &[u8], observed_at: DateTime<Utc>) -> Self {
        let item = Sha256::digest(record);
        Self::other(
            SourceKind::ImportItem,
            import_session(source_id),
            format!("item:{}", hex(&item[..16])),
            observed_at,
        )
    }

    /// The marker a dispatch run's write carries when its bus message handed
    /// it an incomplete upstream turn identity (see
    /// [`SourceKind::UpstreamUnknown`]). Keyed by the run's own key.
    pub fn upstream_unknown(run_key: &str, observed_at: DateTime<Utc>) -> Self {
        Self::other(
            SourceKind::UpstreamUnknown,
            UNTRACKED_SESSION,
            format!("upstream:run:{}", run_key.trim()),
            observed_at,
        )
    }

    /// Reject a malformed source (fail closed: the write does not happen).
    pub fn validate(&self) -> Result<(), String> {
        if self.kind == SourceKind::UpstreamUnknown {
            if self.session != UNTRACKED_SESSION || self.seq.is_some() {
                return Err("an upstream-unknown marker has a fixed session and no seq".into());
            }
            return check_key("message", &self.message);
        }
        if self.kind == SourceKind::System {
            return Err("SourceKind::System is only produced by Provenance::System".into());
        }
        check_key("session", &self.session)?;
        check_key("message", &self.message)?;
        if self.session.starts_with(SYSTEM_SESSION_PREFIX) || self.session == UNTRACKED_SESSION {
            return Err(format!("session uses a reserved prefix: {}", self.session));
        }
        match (self.kind, self.seq) {
            (SourceKind::ChannelMessage, Some(s)) if s >= 0 => {}
            (SourceKind::ChannelMessage, _) => {
                return Err("a channel message source needs a non-negative seq".into());
            }
            (_, Some(_)) => return Err("only channel message sources carry a seq".into()),
            _ => {}
        }
        if let Some(h) = &self.content_hash
            && (h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err("content_hash must be 64 lowercase hex chars".into());
        }
        Ok(())
    }

    /// Audit-safe digest of this source.
    pub fn digest(&self, agent_id: &str) -> String {
        source_digest(agent_id, &self.session, &self.message, "")
    }
}

fn check_key(name: &str, v: &str) -> Result<(), String> {
    if v.trim().is_empty() {
        return Err(format!("source {name} is empty"));
    }
    if v.len() > MAX_SOURCE_KEY_BYTES {
        return Err(format!(
            "source {name} longer than {MAX_SOURCE_KEY_BYTES} bytes"
        ));
    }
    if v.chars().any(|c| c.is_control()) {
        return Err(format!("source {name} contains a control character"));
    }
    Ok(())
}

/// Where a memory write came from. A required argument of every engine write
/// API (design decision D4): a producer that forgets to pass it does not
/// compile, and a conversation-derived write labelled `System` would be a
/// resurrection path, which `lineage::tests` scans for.
#[derive(Debug, Clone, PartialEq)]
pub enum Provenance {
    /// Direct sources (at least one).
    Sources(Vec<SourceRef>),
    /// Derived from existing memory rows (all their sources are inherited),
    /// plus optional direct sources. Parents must exist and not be forgotten.
    Derived {
        parents: Vec<String>,
        extra: Vec<SourceRef>,
    },
    /// Non-conversation content; cannot be forgotten by a conversation source.
    System { producer: &'static str },
}

impl Provenance {
    /// One direct source.
    pub fn source(s: SourceRef) -> Self {
        Self::Sources(vec![s])
    }

    /// Derived from `parents` only.
    pub fn derived(parents: Vec<String>) -> Self {
        Self::Derived {
            parents,
            extra: Vec::new(),
        }
    }

    /// Provenance for tests. It does not exist in a production build: only
    /// this crate's tests and builds with the `test-hooks` feature (other
    /// crates enable it through a dev-dependency) have it.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn test_only() -> Self {
        Self::System {
            producer: TEST_ONLY_PRODUCER,
        }
    }

    /// The same provenance with `parents` added (used where the engine itself
    /// knows the rows a write is derived from, e.g. decision resolution).
    pub(crate) fn with_parents(self, more: Vec<String>) -> Self {
        match self {
            Self::Sources(extra) => Self::Derived {
                parents: more,
                extra,
            },
            Self::Derived { mut parents, extra } => {
                parents.extend(more);
                Self::Derived { parents, extra }
            }
            Self::System { .. } => Self::Derived {
                parents: more,
                extra: Vec::new(),
            },
        }
    }
}

/// Why a write was refused by the source fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceReason {
    /// A source of the write (direct, inherited or reaffirming) was forgotten.
    SourceForgotten,
    /// A parent row was deleted by forget-source.
    ParentForgotten,
    /// A parent row does not exist (design D9: unknown source ⇒ no write).
    ParentMissing,
    /// More than [`MAX_LINEAGE_SOURCES`] sources.
    LineageOverflow,
}

impl FenceReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceForgotten => "source_forgotten",
            Self::ParentForgotten => "parent_forgotten",
            Self::ParentMissing => "parent_missing",
            Self::LineageOverflow => "lineage_overflow",
        }
    }
}

/// A refused write. Carries only digests and ids, never source keys or content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FenceRefusal {
    pub reason: FenceReason,
    /// Digest of the forgotten source ([`FenceReason::SourceForgotten`]).
    pub source_digest: Option<String>,
    /// The parent id ([`FenceReason::ParentForgotten`] / `ParentMissing`).
    pub parent_id: Option<String>,
}

impl FenceRefusal {
    /// The typed error an error-returning write API reports this refusal as.
    pub fn into_error(self) -> duduclaw_core::error::DuDuClawError {
        duduclaw_core::error::DuDuClawError::SourceFenced {
            reason: self.reason.as_str().to_string(),
            source_digest: self.source_digest.clone(),
            detail: self.to_string(),
        }
    }
}

impl std::fmt::Display for FenceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason.as_str())?;
        if let Some(d) = &self.source_digest {
            write!(f, " (source {d})")?;
        }
        if let Some(p) = &self.parent_id {
            write!(f, " (parent {p})")?;
        }
        Ok(())
    }
}

/// Result of [`store_fact_outcome`](crate::engine::SqliteMemoryEngine::store_fact_outcome).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FactWriteOutcome {
    Stored(String),
    Fenced(FenceRefusal),
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_rekey;
