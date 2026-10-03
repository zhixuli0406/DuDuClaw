//! Opt-in CCR configuration for provider-agnostic tool loops.
//!
//! Ambiguous or malformed configuration disables CCR. The tool loop itself
//! commits originals before emitting markers and returns full results on any
//! store failure.

use std::path::Path;
use std::sync::Arc;

use duduclaw_llm::{
    CcrBoundSourceValidator, CcrDeliveryLease, CcrError, CcrScope, CcrSourceArtifact,
};
use duduclaw_memory::causal::{CausalCcrDeliveryLease, CausalStore, EvidenceScope};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct ActiveBoundSourceValidator {
    causal_store: CausalStore,
    wiki: crate::wiki_mcp_source::WikiSourceAuthority,
}

#[derive(Debug)]
struct GatewayCausalDeliveryLease(CausalCcrDeliveryLease);

pub(crate) fn bound_source_validator(home: &Path) -> Arc<dyn CcrBoundSourceValidator> {
    Arc::new(ActiveBoundSourceValidator {
        causal_store: CausalStore::new(home.join("memory.db")),
        wiki: crate::wiki_mcp_source::WikiSourceAuthority::new(home),
    })
}

impl CcrDeliveryLease for GatewayCausalDeliveryLease {
    fn still_valid(&self) -> bool {
        self.0.still_valid()
    }
}

impl CcrBoundSourceValidator for ActiveBoundSourceValidator {
    fn valid(&self, scope: &CcrScope, artifact: &CcrSourceArtifact, saved_sha256: &str) -> bool {
        if artifact.connector == "wiki_agent" {
            return self.wiki.valid_bound(scope, artifact, saved_sha256);
        }
        if artifact.connector != "causal" {
            // This gateway has no current-source authority for other bound
            // connectors. Generic unbound MCP entries never reach this hook.
            return false;
        }
        // CausalStore::open creates a missing file. An unavailable authority
        // must refuse the read without creating a replacement database.
        if !self.causal_store.path().is_file() {
            return false;
        }
        let expected_acl = format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.source_acl))
        );
        if artifact.acl_revision != expected_acl {
            return false;
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.source_acl.clone(),
        };
        matches!(
            self.causal_store.read_artifact_metadata(&evidence_scope, &artifact.artifact_id),
            Ok(metadata) if metadata.version == artifact.version
                && metadata.content_sha256 == saved_sha256
        )
    }

    fn acquire_delivery_guard(
        &self,
        scope: &CcrScope,
        artifact: &CcrSourceArtifact,
        saved_sha256: &str,
    ) -> Result<Option<Arc<dyn CcrDeliveryLease>>, CcrError> {
        if artifact.connector == "wiki_agent" {
            return self
                .wiki
                .acquire_delivery_lease(scope, artifact, saved_sha256)
                .map(Some)
                .map_err(|_| CcrError::Revoked);
        }
        if artifact.connector != "causal" {
            return Err(CcrError::Revoked);
        }
        let source_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.source_acl.clone(),
        };
        let lease = self
            .causal_store
            .acquire_ccr_delivery_lease(
                &source_scope,
                &artifact.artifact_id,
                &artifact.version,
                saved_sha256,
                &artifact.acl_revision,
            )
            .map_err(|_| CcrError::Revoked)?;
        Ok(Some(Arc::new(GatewayCausalDeliveryLease(lease))))
    }
}

tokio::task_local! {
    /// Committed native-tool originals from one channel reply. This scope is
    /// absent from background/utility calls, so they cannot write into a
    /// customer conversation's history by accident.
    pub static SAVED_RESULT_COLLECTOR: std::sync::Arc<std::sync::Mutex<Vec<duduclaw_llm::CcrSavedResult>>>;
    /// Source leases for every native tool loop in one customer reply. The
    /// reply builder retains these until the channel finishes sending.
    pub static DELIVERY_GUARD_COLLECTOR: std::sync::Arc<std::sync::Mutex<Vec<duduclaw_llm::CcrDeliveryGuards>>>;
    /// Set only by WebChat after JWT/widget authentication and resume ownership
    /// checks. The connection suffix in CHANNEL_REPLY_USER_ID is not a durable
    /// principal across authenticated WebChat reconnects.
    pub static AUTHENTICATED_WEBCHAT_PRINCIPAL: String;
}

/// A native tool loop must not return a source-derived answer unless its
/// leases can be handed to the reply builder. An empty set needs no scope.
///
/// Async on purpose: every runtime adapter calls this once per loop outcome,
/// and revalidation opens SQLite (3-4 connections per entry guard) plus any
/// file reads a source authority performs. [`CcrDeliveryGuards::still_valid`]
/// moves that onto the blocking pool, so a tool-loop answer no longer stalls
/// the reactor for the length of a disk round-trip. An empty guard set still
/// answers inline without touching the pool. There is no `_blocking`
/// companion: every call site is async.
pub async fn capture_delivery_guards(guards: duduclaw_llm::CcrDeliveryGuards) -> bool {
    if guards.is_empty() {
        return true;
    }
    // Task-locals survive this await: `still_valid` offloads to a *separate*
    // blocking task, while `DELIVERY_GUARD_COLLECTOR` below is read back on
    // this task, inside the caller's scope.
    if !guards.still_valid().await {
        return false;
    }
    DELIVERY_GUARD_COLLECTOR
        .try_with(|collector| collector.lock().map(|mut held| held.push(guards)).is_ok())
        .unwrap_or(false)
}

pub fn capture_saved_results(records: Vec<duduclaw_llm::CcrSavedResult>) {
    if records.is_empty() {
        return;
    }
    let _ = SAVED_RESULT_COLLECTOR.try_with(|collector| {
        if let Ok(mut saved) = collector.lock() {
            saved.extend(records);
        }
    });
}

pub fn take_saved_results() -> Vec<duduclaw_llm::CcrSavedResult> {
    SAVED_RESULT_COLLECTOR
        .try_with(|collector| {
            collector
                .lock()
                .map(|mut saved| std::mem::take(&mut *saved))
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

async fn currently_valid(
    runtime: duduclaw_llm::CcrRuntime,
    candidates: Vec<duduclaw_llm::CcrSavedResult>,
    limit: usize,
) -> Result<Vec<duduclaw_llm::CcrSavedResult>, String> {
    tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter(|saved| {
                runtime
                    .valid_saved_reference(
                        &saved.scope,
                        &saved.id,
                        &saved.source_tool,
                        &saved.source_call_id,
                        saved.original_bytes,
                    )
                    .unwrap_or(false)
            })
            .take(limit)
            .collect()
    })
    .await
    .map_err(|error| format!("CCR validation task failed: {error}"))
}

/// Persist this reply's committed handles only when their source user turn is
/// still active and their originals still pass the current policy.
pub async fn persist_collected_results(
    sessions: &crate::session::SessionManager,
    home: &Path,
    agent_id: &str,
    session_id: &str,
    user_message_id: i64,
) -> Result<(), String> {
    let candidates = take_saved_results();
    if candidates.is_empty() {
        return Ok(());
    }
    let Some(runtime) = for_agent(home, agent_id) else {
        return Ok(());
    };
    let valid = currently_valid(runtime, candidates, 100).await?;
    sessions
        .append_ccr_saved_results(session_id, user_message_id, &valid)
        .await
        .map_err(|error| error.to_string())
}

/// Render only revalidated opaque IDs. The raw tool output never enters the
/// session prompt; retrieval itself performs a fresh authorization check.
pub async fn historical_reference_note(
    sessions: &crate::session::SessionManager,
    home: &Path,
    agent_id: &str,
    session_id: &str,
    current_task: &str,
) -> Option<String> {
    let runtime = for_agent(home, agent_id)?;
    let candidates = sessions.get_ccr_saved_results(session_id, 20).await.ok()?;
    // The session query supplies newest-first order. Only the twenty saved
    // candidates are scored; the scorer sees their private originals but
    // returns a number, and each ID is checked again before display.
    let task: String = current_task.chars().take(4_096).collect();
    let valid = tokio::task::spawn_blocking(move || {
        let mut ranked = candidates
            .into_iter()
            .take(20)
            .enumerate()
            .filter_map(|(recency, saved)| {
                runtime
                    .saved_reference_task_score(
                        &saved.scope,
                        &saved.id,
                        &saved.source_tool,
                        &saved.source_call_id,
                        saved.original_bytes,
                        &task,
                    )
                    .ok()
                    .flatten()
                    .map(|score| (score, recency, saved))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        ranked
            .into_iter()
            .filter_map(|(_, _, saved)| {
                runtime
                    .valid_saved_reference(
                        &saved.scope,
                        &saved.id,
                        &saved.source_tool,
                        &saved.source_call_id,
                        saved.original_bytes,
                    )
                    .ok()
                    .filter(|valid| *valid)
                    .map(|_| saved)
            })
            .take(5)
            .collect::<Vec<_>>()
    })
    .await
    .ok()?;
    if valid.is_empty() {
        return None;
    }
    let references = valid
        .iter()
        .map(|saved| format!("- id={} bytes={}", saved.id, saved.original_bytes))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "## Historical CCR originals\nThese handles passed a recent availability check. Retrieval rechecks authorization and source state, and may still refuse a handle. Retrieve an original by ID only when needed for this task.\n{references}"
    ))
}

/// DuDuClaw's own MCP server, as named in `.mcp.json` / `ToolRegistry`.
pub const BUILTIN_SERVER: &str = "duduclaw";

/// The built-in read-only tools whose results are routinely large enough for
/// CCR to be worth anything (X1 方案 3).
///
/// Every entry is **read-only** and returns *retrievable* content: a database
/// page, a file, a fetched page, a wiki page, a memory hit. A tool that
/// mutates state, or whose result is a summary of the agent's own words, is
/// deliberately absent — CCR's promise is "the exact original is still
/// there", which only means something for content that came from a source.
///
/// Names are the exact registered MCP tool names (`web_fetch_cached`, not
/// `web_fetch`), verified against `duduclaw-cli/src/mcp.rs`'s catalog.
///
/// Merging these into `allowed_sources` changes nothing while
/// `[ccr] enabled = false` (the default): the allowlist is only populated
/// when CCR is on, and `min_compress_bytes` (4096) still means a small result
/// is byte-identical either way.
pub const BUILTIN_CCR_SOURCE_TOOLS: &[&str] = &[
    "db_select",
    "db_query",
    "csv_read",
    "xlsx_read",
    "file_read",
    "web_fetch_cached",
    "web_extract",
    "wiki_read",
    "memory_search",
    "memory_fetch_batch",
];

/// Resolve the effective allowlist: the operator's `[[ccr.allowed_sources]]`
/// plus, when `builtin_sources` is on, every entry of
/// [`BUILTIN_CCR_SOURCE_TOOLS`] on the `duduclaw` server.
///
/// Duplicates are harmless (`restrict_sources` collects into a set) but are
/// removed here anyway so the returned list is exactly what a reader expects.
/// `builtin_sources = false` returns the operator list unchanged, byte for
/// byte, which is what the regression test pins.
pub fn effective_allowed_sources(
    operator: Vec<(String, String)>,
    builtin_sources: bool,
) -> Vec<(String, String)> {
    if !builtin_sources {
        return operator;
    }
    let mut out = operator;
    for tool in BUILTIN_CCR_SOURCE_TOOLS {
        let entry = (BUILTIN_SERVER.to_string(), (*tool).to_string());
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct CcrSettings {
    enabled: bool,
    ttl_seconds: i64,
    max_entries: usize,
    min_compress_bytes: usize,
    allowed_sources: Vec<CcrAllowedSource>,
    /// X1 方案 3 — fold DuDuClaw's own high-output read-only tools into the
    /// allowlist. **Default true**, and inert while `enabled = false`.
    builtin_sources: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CcrAllowedSource {
    server: String,
    tool: String,
}

impl Default for CcrSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_seconds: 1_800,
            max_entries: 1_000,
            min_compress_bytes: 4_096,
            allowed_sources: Vec::new(),
            builtin_sources: true,
        }
    }
}

/// Ids that name "we could not identify the human", not a human.
///
/// A webhook adapter that cannot read a sender id used to substitute a literal
/// placeholder, which hashes to one stable `source_acl` shared by every
/// anonymous sender on that channel — exactly the cross-user retrieval the
/// principal hash exists to prevent. Compared with exact, ASCII-case-
/// insensitive equality after trimming (never `contains`, per the project's
/// unanchored-match convention).
const PLACEHOLDER_PRINCIPALS: &[&str] = &["unknown", "anonymous", "system"];

/// Whether `principal_id` is blank or one of the reserved placeholder names,
/// i.e. cannot stand for one identified human.
pub fn is_placeholder_principal(principal_id: &str) -> bool {
    let trimmed = principal_id.trim();
    trimmed.is_empty()
        || PLACEHOLDER_PRINCIPALS
            .iter()
            .any(|reserved| trimmed.eq_ignore_ascii_case(reserved))
}

/// Launder a channel sender id into a CCR principal.
///
/// Returns the sender id unchanged when it identifies a human, and `""` when
/// it does not — an empty principal makes [`source_acl_for_principal`] return
/// `None`, so CCR is simply off for that turn (fail-closed) instead of pooling
/// every anonymous sender into one shared retrieval scope.
///
/// Channel adapters keep their own placeholder value for logs, session keys,
/// rate-limit keys and audit fields; only the reply pipeline's `user_id`
/// argument (the CCR principal) goes through here.
pub fn reply_principal_for_sender(sender_id: &str) -> &str {
    if is_placeholder_principal(sender_id) {
        ""
    } else {
        sender_id
    }
}

/// Stable exact-scope tag shared with the owner-operated revocation command.
/// The hash discriminates callers; it does not authenticate them.
pub fn source_acl_for_principal(
    agent_id: &str,
    session_id: &str,
    principal_id: &str,
) -> Option<String> {
    if agent_id.trim().is_empty()
        || session_id.trim().is_empty()
        || session_id == "mcp-session"
        || is_placeholder_principal(principal_id)
    {
        return None;
    }
    let principal_hash = format!("{:x}", Sha256::digest(principal_id.trim().as_bytes()));
    Some(format!(
        "agent:{agent_id}:session:{session_id}:principal-sha256:{principal_hash}"
    ))
}

pub fn for_agent(home: &Path, agent_id: &str) -> Option<duduclaw_llm::CcrRuntime> {
    if agent_id.trim().is_empty() {
        return None;
    }
    // A model-visible retrieval tool must be scoped to the authenticated
    // caller, not only to a potentially reused agent/channel session. Utility
    // turns without a caller keep CCR disabled until they have their own
    // explicit principal policy.
    let caller = crate::claude_runner::CHANNEL_REPLY_USER_ID
        .try_with(|id| id.trim().to_owned())
        .ok()
        .filter(|id| !id.is_empty())?;
    let session_id = crate::redaction_proxy::current_session_id();
    if session_id == "mcp-session" || session_id.trim().is_empty() {
        return None;
    }
    let principal = match AUTHENTICATED_WEBCHAT_PRINCIPAL.try_with(Clone::clone) {
        Ok(auth_user) => {
            let owner_tag = crate::webchat::webchat_owner_tag(&auth_user);
            if auth_user.trim().is_empty()
                || !caller.starts_with(&format!("webchat:{owner_tag}:"))
                || !crate::webchat::owns_webchat_session(&session_id, &owner_tag)
            {
                return None;
            }
            format!("webchat-auth:{auth_user}")
        }
        Err(_) => caller,
    };
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let root = raw.parse::<toml::Table>().ok()?;
    let settings: CcrSettings = root.get("ccr")?.clone().try_into().ok()?;
    if settings.ttl_seconds <= 0
        || settings.max_entries == 0
        || settings
            .allowed_sources
            .iter()
            .any(|source| source.server.trim().is_empty() || source.tool.trim().is_empty())
    {
        return None;
    }
    let source_acl = source_acl_for_principal(agent_id, &session_id, &principal)?;
    let scope = duduclaw_llm::CcrScope {
        tenant_id: "local".into(),
        agent_id: agent_id.to_owned(),
        source_acl,
        session_id,
    };
    let store = duduclaw_llm::CcrStore::new(home.join("ccr").join("ccr.db"))
        .with_limits(settings.ttl_seconds, settings.max_entries);
    let allowed_sources: Vec<_> = if settings.enabled {
        effective_allowed_sources(
            settings
                .allowed_sources
                .into_iter()
                .map(|source| (source.server, source.tool))
                .collect(),
            settings.builtin_sources,
        )
    } else {
        Vec::new()
    };
    let active = !allowed_sources.is_empty();
    let mut runtime = duduclaw_llm::CcrRuntime::new(store, scope)
        .restrict_sources(allowed_sources)
        // W2-E: the llm crate holds no sentinel of its own — this is the one
        // production point that tells a `CcrRuntime` which marker belongs to
        // the gateway process, so a composer-rendered never-trim section is
        // still exempt from CCR compression while a tool result that merely
        // prints `## Constraints` is not.
        .with_protected_sentinel(duduclaw_core::protected_section::process_sentinel())
        .with_bound_source_validator(bound_source_validator(home));
    runtime.min_compress_bytes = settings.min_compress_bytes.max(1_024);
    runtime.purge_disallowed_sources().ok()?;
    active.then_some(runtime)
}

/// Source-text scanning shared by the per-adapter `ccr_principal` regression
/// tests.
///
/// The offending call sites live inside 200+ line async webhook handlers that
/// cannot be driven from a unit test, so the guard is structural: read the
/// adapter's own source and assert the argument in the CCR-principal position
/// is laundered through [`reply_principal_for_sender`].
#[cfg(test)]
pub(crate) mod source_scan {
    /// Every top-level argument at `arg_index` (zero-based) of every call to
    /// `needle` in `src`, trimmed and with a leading `&` removed.
    pub(crate) fn call_args_at<'a>(src: &'a str, needle: &str, arg_index: usize) -> Vec<&'a str> {
        let mut found = Vec::new();
        let mut rest = src;
        while let Some(at) = rest.find(needle) {
            let after = &rest[at + needle.len()..];
            let args = balanced_args(after)
                .unwrap_or_else(|| panic!("unbalanced argument list after `{needle}`"));
            if let Some(arg) = split_top_level_commas(args).get(arg_index) {
                found.push(arg.trim().trim_start_matches('&'));
            }
            rest = &rest[at + needle.len()..];
        }
        found
    }

    /// The argument list (without the outer parentheses) starting at `after`.
    fn balanced_args(after: &str) -> Option<&str> {
        let mut depth = 1usize;
        for (index, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&after[..index]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn split_top_level_commas(args: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut depth = 0usize;
        let mut start = 0usize;
        for (index, ch) in args.char_indices() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    parts.push(&args[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        parts.push(&args[start..]);
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use duduclaw_llm::{CcrEntry, CcrRuntime};

    // ── X1 方案 3: built-in high-output source routes ────────────────────

    #[test]
    fn builtin_sources_merge_onto_the_operator_allowlist() {
        let operator = vec![("support-mcp".to_string(), "search".to_string())];
        let merged = effective_allowed_sources(operator.clone(), true);
        assert!(
            merged.starts_with(&operator),
            "the operator's own routes stay first and unchanged"
        );
        for tool in BUILTIN_CCR_SOURCE_TOOLS {
            assert!(
                merged.contains(&(BUILTIN_SERVER.to_string(), (*tool).to_string())),
                "{tool} must be routable"
            );
        }
        assert_eq!(merged.len(), operator.len() + BUILTIN_CCR_SOURCE_TOOLS.len());
    }

    #[test]
    fn builtin_sources_off_is_byte_identical_to_the_operator_list() {
        let operator = vec![
            ("support-mcp".to_string(), "search".to_string()),
            ("duduclaw".to_string(), "wiki_read".to_string()),
        ];
        assert_eq!(
            effective_allowed_sources(operator.clone(), false),
            operator,
            "builtin_sources = false must change nothing"
        );
    }

    #[test]
    fn a_builtin_route_the_operator_already_listed_is_not_duplicated() {
        let operator = vec![("duduclaw".to_string(), "wiki_read".to_string())];
        let merged = effective_allowed_sources(operator, true);
        assert_eq!(
            merged
                .iter()
                .filter(|(s, t)| s == "duduclaw" && t == "wiki_read")
                .count(),
            1
        );
    }

    /// A removed tool name routes nothing; `wiki_read` covers both wikis.
    #[test]
    fn builtin_routes_name_no_removed_tool() {
        for tool in BUILTIN_CCR_SOURCE_TOOLS {
            assert!(
                duduclaw_core::tool_catalog::removed_mcp_tool(tool).is_none(),
                "{tool} was removed"
            );
        }
        assert!(BUILTIN_CCR_SOURCE_TOOLS.contains(&"wiki_read"));
    }

    #[test]
    fn builtin_routes_name_only_read_only_retrievable_tools() {
        // A write tool, or one that echoes the agent's own summary back, must
        // never become a CCR source: "the exact original is still there" only
        // means something for content that came from a source.
        for tool in BUILTIN_CCR_SOURCE_TOOLS {
            assert!(
                !duduclaw_core::grounding::SELF_ECHO_TOOL_NAMES.contains(tool),
                "{tool} echoes the agent's own text"
            );
            for banned in ["write", "create", "update", "delete", "send", "spawn"] {
                assert!(
                    !tool.contains(banned),
                    "{tool} looks like a mutation ({banned})"
                );
            }
        }
    }

    #[test]
    fn builtin_settings_default_on_and_are_overridable() {
        let default: CcrSettings = toml::from_str("").unwrap();
        assert!(default.builtin_sources, "default is on");
        assert!(!default.enabled, "but CCR itself stays off by default");
        let off: CcrSettings = toml::from_str("builtin_sources = false\n").unwrap();
        assert!(!off.builtin_sources);
    }

    #[tokio::test]
    async fn ccr_disabled_keeps_every_builtin_route_out_of_the_allowlist() {
        let home = tempfile::tempdir().unwrap();
        // `enabled = false` with builtin_sources defaulting on: the runtime
        // must still be inactive, i.e. `for_agent` returns None.
        std::fs::write(home.path().join("config.toml"), "[ccr]\nenabled = false\n").unwrap();
        assert!(
            scoped(home.path(), "user-1", "session-1").await.is_none(),
            "builtin sources must not switch CCR on by themselves"
        );
    }

    #[tokio::test]
    async fn enabled_ccr_routes_builtin_tools_without_any_operator_entry() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[ccr]\nenabled = true\n").unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1")
            .await
            .expect("builtin sources alone make the runtime active");
        assert!(
            runtime
                .source_key_for_call(Some(BUILTIN_SERVER), "db_select")
                .is_some(),
            "db_select must resolve to a CCR source route"
        );
        assert!(
            runtime
                .source_key_for_call(Some(BUILTIN_SERVER), "tasks_complete")
                .is_none(),
            "a tool outside the built-in list stays unroutable"
        );
    }

    #[tokio::test]
    async fn builtin_sources_false_with_no_operator_entry_leaves_ccr_inactive() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\nenabled = true\nbuiltin_sources = false\n",
        )
        .unwrap();
        assert!(
            scoped(home.path(), "user-1", "session-1").await.is_none(),
            "an empty allowlist denies every source, exactly as before"
        );
    }

    fn bound_source(
        runtime: &CcrRuntime,
        causal: &CausalStore,
        external_id: &str,
        content: &str,
    ) -> (String, CcrEntry) {
        let scope = EvidenceScope {
            tenant_id: runtime.scope.tenant_id.clone(),
            acl: runtime.scope.source_acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &scope,
                "ticket",
                external_id,
                "v1",
                external_id,
                content,
                1,
                i64::MAX / 2,
            )
            .unwrap();
        let binding = CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: artifact.id.clone(),
            version: artifact.version,
            acl_revision: format!(
                "immutable-acl-sha256:{:x}",
                Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
            ),
        };
        let source_tool = runtime
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let entry = runtime
            .store
            .put_bound(&runtime.scope, &source_tool, external_id, content, &binding)
            .unwrap();
        (artifact.id, entry)
    }

    async fn scoped(home: &Path, user: &str, session: &str) -> Option<duduclaw_llm::CcrRuntime> {
        crate::claude_runner::CHANNEL_REPLY_USER_ID
            .scope(user.to_owned(), async {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some(session.to_owned()), async { for_agent(home, "agent") })
                    .await
            })
            .await
    }

    #[tokio::test]
    async fn native_bound_reads_recheck_causal_lifecycle_and_keep_generic_results() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n").unwrap();
        // This enters the real gateway constructor with a principal-derived ACL.
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let content = "evidence for the live ticket";
        let (live_id, live) = bound_source(&runtime, &causal, "live", content);
        let (invalid_id, invalid) = bound_source(&runtime, &causal, "invalid", content);
        let (erased_id, erased) = bound_source(&runtime, &causal, "erased", content);
        let (expired_id, expired) = bound_source(&runtime, &causal, "expired", content);
        let source_tool = runtime
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let generic = runtime
            .store
            .put(&runtime.scope, &source_tool, "generic", "generic evidence")
            .unwrap();

        let exact = EvidenceScope {
            tenant_id: runtime.scope.tenant_id.clone(),
            acl: runtime.scope.source_acl.clone(),
        };
        causal.invalidate_artifact(&exact, &invalid_id).unwrap();
        causal.erase_artifact(&exact, &erased_id).unwrap();
        let conn = rusqlite::Connection::open(causal.path()).unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET retention_at=0 WHERE id=?1",
            [&expired_id],
        )
        .unwrap();
        drop(conn);

        for entry in [&invalid, &erased, &expired] {
            assert!(runtime.retrieve(&entry.id, None, 0, 128).is_err());
            assert!(
                !runtime
                    .valid_saved_reference(
                        &runtime.scope,
                        &entry.id,
                        &source_tool,
                        &entry.source_call_id,
                        entry.content_bytes
                    )
                    .unwrap()
            );
            assert_eq!(
                runtime
                    .saved_reference_task_score(
                        &runtime.scope,
                        &entry.id,
                        &source_tool,
                        &entry.source_call_id,
                        entry.content_bytes,
                        "live ticket evidence",
                    )
                    .unwrap(),
                None
            );
        }
        assert!(
            runtime
                .saved_reference_task_score(
                    &runtime.scope,
                    &live.id,
                    &source_tool,
                    &live.source_call_id,
                    live.content_bytes,
                    "live ticket evidence",
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(
            runtime.retrieve(&live.id, None, 0, 128).unwrap().text,
            content
        );
        assert_eq!(
            runtime.retrieve(&generic.id, None, 0, 128).unwrap().text,
            "generic evidence"
        );
        let hits = runtime.find("evidence", 5).unwrap();
        assert!(hits.iter().any(|hit| hit.id == live.id));
        assert!(hits.iter().any(|hit| hit.id == generic.id));
        assert!(
            hits.iter()
                .all(|hit| ![&invalid.id, &erased.id, &expired.id].contains(&&hit.id))
        );

        let other = scoped(home.path(), "user-2", "session-1").await.unwrap();
        assert!(other.retrieve(&live.id, None, 0, 128).is_err());
        assert!(other.find("evidence", 5).unwrap().is_empty());
        assert!(
            !other
                .valid_saved_reference(
                    &runtime.scope,
                    &live.id,
                    &source_tool,
                    "live",
                    live.content_bytes
                )
                .unwrap()
        );

        std::fs::rename(causal.path(), home.path().join("memory.unavailable")).unwrap();
        assert!(runtime.retrieve(&live.id, None, 0, 128).is_err());
        assert!(
            runtime
                .find("evidence", 5)
                .unwrap()
                .iter()
                .all(|hit| hit.id != live.id)
        );
        assert_eq!(
            runtime.retrieve(&generic.id, None, 0, 128).unwrap().text,
            "generic evidence"
        );
        std::fs::rename(home.path().join("memory.unavailable"), causal.path()).unwrap();
        assert_eq!(
            runtime.retrieve(&live.id, None, 0, 128).unwrap().text,
            content
        );
        assert!(causal.source_text(&exact, &live_id).is_ok());
    }

    #[tokio::test]
    async fn native_causal_retrieval_retains_source_lease_until_chunk_is_released() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n").unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (id, entry) = bound_source(&runtime, &causal, "leased", "protected original");
        let scope = EvidenceScope {
            tenant_id: runtime.scope.tenant_id.clone(),
            acl: runtime.scope.source_acl.clone(),
        };
        let chunk = runtime.retrieve(&entry.id, None, 0, 128).unwrap();
        assert_eq!(chunk.text, "protected original");
        let guard = chunk
            .delivery_guard()
            .expect("causal binding must have a lease");
        assert!(guard.still_valid());
        assert!(matches!(
            causal.invalidate_artifact(&scope, &id),
            Err(duduclaw_memory::causal::CausalStoreError::Conflict)
        ));
        assert!(!guard.still_valid());
        assert!(runtime.retrieve(&entry.id, None, 0, 128).is_err());
        drop(chunk);
        drop(guard);
        causal.invalidate_artifact(&scope, &id).unwrap();
        assert!(runtime.retrieve(&entry.id, None, 0, 128).is_err());
    }

    #[tokio::test]
    async fn unsupported_bound_connector_is_never_released() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n").unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let source_tool = runtime
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let unsupported = runtime
            .store
            .put_bound(
                &runtime.scope,
                &source_tool,
                "bound-call",
                "unsupported evidence",
                &CcrSourceArtifact {
                    connector: "support-api".into(),
                    artifact_id: "ticket-42".into(),
                    version: "v1".into(),
                    acl_revision: "acl-v1".into(),
                },
            )
            .unwrap();
        let unbound = runtime
            .store
            .put(
                &runtime.scope,
                &source_tool,
                "unbound-call",
                "ordinary evidence",
            )
            .unwrap();
        assert!(matches!(
            runtime.retrieve(&unsupported.id, None, 0, 128),
            Err(duduclaw_llm::CcrError::Revoked)
        ));
        assert!(
            !runtime
                .valid_saved_reference(
                    &runtime.scope,
                    &unsupported.id,
                    &source_tool,
                    "bound-call",
                    unsupported.content_bytes
                )
                .unwrap()
        );
        assert!(runtime.find("unsupported evidence", 5).unwrap().is_empty());
        assert_eq!(
            runtime.retrieve(&unbound.id, None, 0, 128).unwrap().text,
            "ordinary evidence"
        );
    }

    #[tokio::test]
    async fn find_skips_five_newer_stale_bound_hits_before_final_limit() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n").unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let causal = CausalStore::new(home.path().join("memory.db"));
        let (_, live) = bound_source(&runtime, &causal, "old-live", "evidence live");
        let exact = EvidenceScope {
            tenant_id: runtime.scope.tenant_id.clone(),
            acl: runtime.scope.source_acl.clone(),
        };
        for index in 0..5 {
            let name = format!("new-stale-{index}");
            let (id, _) = bound_source(&runtime, &causal, &name, "evidence stale");
            causal.invalidate_artifact(&exact, &id).unwrap();
        }
        let hits = runtime.find("evidence", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, live.id);
    }

    #[tokio::test]
    async fn committed_reference_survives_turn_then_disappears_for_other_user_or_revocation() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n",
        ).unwrap();
        let sessions =
            crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
        sessions.get_or_create("session-1", "agent").await.unwrap();
        let user_message_id = sessions
            .append_message_with_id("session-1", "user", "Find SLA evidence", 4)
            .await
            .unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let source_tool = runtime
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let original = "private SLA evidence\n".repeat(400);
        let entry = runtime
            .store
            .put(&runtime.scope, &source_tool, "call-1", &original)
            .unwrap();
        let saved = duduclaw_llm::CcrSavedResult {
            scope: runtime.scope.clone(),
            source_tool: source_tool.clone(),
            source_call_id: "call-1".into(),
            id: entry.id.clone(),
            original_bytes: entry.content_bytes,
            expires_at: entry.expires_at,
        };

        crate::channel_reply::reply_identity_scope("session-1", "user-1", async {
            let collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            SAVED_RESULT_COLLECTOR
                .scope(collector, async {
                    capture_saved_results(vec![saved]);
                    persist_collected_results(
                        &sessions,
                        home.path(),
                        "agent",
                        "session-1",
                        user_message_id,
                    )
                    .await
                    .unwrap();
                    let note = historical_reference_note(
                        &sessions,
                        home.path(),
                        "agent",
                        "session-1",
                        "SLA evidence",
                    )
                    .await
                    .unwrap();
                    assert!(note.contains(&entry.id));
                    assert!(!note.contains("private SLA evidence"));
                    assert_eq!(sessions.get_messages("session-1").await.unwrap().len(), 1);
                })
                .await;
        })
        .await;

        let other_user_note = crate::claude_runner::CHANNEL_REPLY_USER_ID
            .scope("user-2".to_owned(), async {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some("session-1".to_owned()), async {
                        historical_reference_note(
                            &sessions,
                            home.path(),
                            "agent",
                            "session-1",
                            "SLA evidence",
                        )
                        .await
                    })
                    .await
            })
            .await;
        assert!(other_user_note.is_none());

        runtime
            .store
            .revoke_source_call(&runtime.scope, &source_tool, "call-1")
            .unwrap();
        let revoked_note = crate::claude_runner::CHANNEL_REPLY_USER_ID
            .scope("user-1".to_owned(), async {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some("session-1".to_owned()), async {
                        historical_reference_note(
                            &sessions,
                            home.path(),
                            "agent",
                            "session-1",
                            "SLA evidence",
                        )
                        .await
                    })
                    .await
            })
            .await;
        assert!(revoked_note.is_none());
    }

    #[tokio::test]
    async fn historical_note_reranks_only_live_same_scope_handles_for_current_task() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n",
        )
        .unwrap();
        let sessions =
            crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
        sessions.get_or_create("session-1", "agent").await.unwrap();
        let turn = sessions
            .append_message_with_id("session-1", "user", "先前的工具結果", 1)
            .await
            .unwrap();
        let runtime = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let route = runtime
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let mut saved = Vec::new();
        let older = runtime
            .store
            .put(
                &runtime.scope,
                &route,
                "older-relevant",
                "客服積壓的 SLA 違約風險提高；refund SLA backlog evidence",
            )
            .unwrap();
        for (entry, call) in std::iter::once((&older, "older-relevant")) {
            saved.push(duduclaw_llm::CcrSavedResult {
                scope: runtime.scope.clone(),
                source_tool: route.clone(),
                source_call_id: call.into(),
                id: entry.id.clone(),
                original_bytes: entry.content_bytes,
                expires_at: entry.expires_at,
            });
        }
        let mut newer = Vec::new();
        for index in 0..5 {
            let call = format!("newer-{index}");
            let entry = runtime
                .store
                .put(
                    &runtime.scope,
                    &route,
                    &call,
                    &format!("客服滿意度報告 {index}; shipping SLA schedule"),
                )
                .unwrap();
            saved.push(duduclaw_llm::CcrSavedResult {
                scope: runtime.scope.clone(),
                source_tool: route.clone(),
                source_call_id: call,
                id: entry.id.clone(),
                original_bytes: entry.content_bytes,
                expires_at: entry.expires_at,
            });
            newer.push(entry);
        }
        let foreign = scoped(home.path(), "user-2", "session-1").await.unwrap();
        let foreign_entry = foreign
            .store
            .put(
                &foreign.scope,
                &route,
                "foreign",
                "客服積壓 refund SLA backlog evidence",
            )
            .unwrap();
        saved.push(duduclaw_llm::CcrSavedResult {
            scope: foreign.scope,
            source_tool: route.clone(),
            source_call_id: "foreign".into(),
            id: foreign_entry.id.clone(),
            original_bytes: foreign_entry.content_bytes,
            expires_at: foreign_entry.expires_at,
        });
        sessions
            .append_ccr_saved_results("session-1", turn, &saved)
            .await
            .unwrap();

        crate::channel_reply::reply_identity_scope("session-1", "user-1", async {
            let note = historical_reference_note(
                &sessions,
                home.path(),
                "agent",
                "session-1",
                "請分析客服積壓與 refund SLA backlog",
            )
            .await
            .unwrap();
            assert_eq!(note.matches("- id=").count(), 5);
            assert!(note.find(&older.id).unwrap() < note.find(&newer[4].id).unwrap());
            assert!(!note.contains(&foreign_entry.id));
            assert!(!note.contains("客服積壓"));
            assert!(!note.contains("refund SLA backlog"));

            let no_task = historical_reference_note(
                &sessions,
                home.path(),
                "agent",
                "session-1",
                "please show me this",
            )
            .await
            .unwrap();
            assert!(!no_task.contains(&older.id));
            assert!(no_task.contains(&newer[4].id));

            runtime
                .store
                .revoke_source_call(&runtime.scope, &route, "older-relevant")
                .unwrap();
            let revoked = historical_reference_note(
                &sessions,
                home.path(),
                "agent",
                "session-1",
                "請分析客服積壓與 refund SLA backlog",
            )
            .await
            .unwrap();
            assert!(!revoked.contains(&older.id));
            assert!(!revoked.contains(&foreign_entry.id));
        })
        .await;
    }

    #[tokio::test]
    async fn webchat_reconnect_keeps_authenticated_principal_but_refuses_other_owner() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n",
        ).unwrap();
        let owner = "account-1";
        let owner_tag = crate::webchat::webchat_owner_tag(owner);
        let session = format!("webchat:webchat:{owner_tag}:old-conn#conv:pilot");
        let old_connection = format!("webchat:{owner_tag}:old-conn");
        let new_connection = format!("webchat:{owner_tag}:new-conn");
        let first = AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                owner.to_owned(),
                crate::channel_reply::reply_identity_scope(&session, &old_connection, async {
                    for_agent(home.path(), "agent").unwrap()
                }),
            )
            .await;
        let source = first
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let entry = first
            .store
            .put(&first.scope, &source, "call-1", "private result")
            .unwrap();
        let sessions =
            crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
        sessions.get_or_create(&session, "agent").await.unwrap();
        let turn = sessions
            .append_message_with_id(&session, "user", "find evidence", 2)
            .await
            .unwrap();
        let saved = duduclaw_llm::CcrSavedResult {
            scope: first.scope.clone(),
            source_tool: source,
            source_call_id: "call-1".into(),
            id: entry.id.clone(),
            original_bytes: entry.content_bytes,
            expires_at: entry.expires_at,
        };
        AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                owner.to_owned(),
                crate::channel_reply::reply_identity_scope(&session, &old_connection, async {
                    let collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                    SAVED_RESULT_COLLECTOR
                        .scope(collector, async {
                            capture_saved_results(vec![saved]);
                            persist_collected_results(
                                &sessions,
                                home.path(),
                                "agent",
                                &session,
                                turn,
                            )
                            .await
                            .unwrap();
                        })
                        .await;
                }),
            )
            .await;

        let resumed = AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                owner.to_owned(),
                crate::channel_reply::reply_identity_scope(&session, &new_connection, async {
                    for_agent(home.path(), "agent").unwrap()
                }),
            )
            .await;
        assert_eq!(first.scope, resumed.scope);
        assert_eq!(
            resumed.retrieve(&entry.id, None, 0, 64).unwrap().text,
            "private result"
        );
        let resumed_note = AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                owner.to_owned(),
                crate::channel_reply::reply_identity_scope(&session, &new_connection, async {
                    historical_reference_note(
                        &sessions,
                        home.path(),
                        "agent",
                        &session,
                        "private result",
                    )
                    .await
                }),
            )
            .await
            .unwrap();
        assert!(resumed_note.contains(&entry.id));
        assert!(!resumed_note.contains("private result"));

        let other_tag = crate::webchat::webchat_owner_tag("account-2");
        let other_connection = format!("webchat:{other_tag}:new-conn");
        let other_owner = AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                "account-2".to_owned(),
                crate::channel_reply::reply_identity_scope(&session, &other_connection, async {
                    for_agent(home.path(), "agent")
                }),
            )
            .await;
        assert!(other_owner.is_none());
        let spoofed_connection = AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                owner.to_owned(),
                crate::channel_reply::reply_identity_scope(
                    &session,
                    "webchat:unrelated:new-conn",
                    async { for_agent(home.path(), "agent") },
                ),
            )
            .await;
        assert!(spoofed_connection.is_none());
    }

    #[tokio::test]
    async fn enabled_ccr_requires_principal_and_separates_same_session_users() {
        let home = tempfile::tempdir().unwrap();
        assert!(for_agent(home.path(), "agent").is_none());
        std::fs::write(home.path().join("config.toml"), "[ccr]\nenabled = true\n").unwrap();
        assert!(for_agent(home.path(), "agent").is_none());
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n",
        )
        .unwrap();
        let first = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let second = scoped(home.path(), "user-2", "session-1").await.unwrap();
        assert_eq!(first.scope.agent_id, "agent");
        assert_eq!(first.scope.session_id, second.scope.session_id);
        assert_ne!(first.scope.source_acl, second.scope.source_acl);
        assert!(!first.scope.source_acl.contains("user-1"));
        assert!(
            first
                .source_key_for_call(Some("support-mcp"), "search")
                .is_some()
        );
        assert!(
            first
                .source_key_for_call(Some("other-mcp"), "search")
                .is_none()
        );
        assert!(first.store.path().ends_with("ccr/ccr.db"));
        let entry = first
            .store
            .put(
                &first.scope,
                &first
                    .source_key_for_call(Some("support-mcp"), "search")
                    .unwrap(),
                "call-1",
                "secret",
            )
            .unwrap();
        assert_eq!(
            first.retrieve(&entry.id, None, 0, 64).unwrap().text,
            "secret"
        );
        assert!(second.retrieve(&entry.id, None, 0, 64).is_err());
        assert!(scoped(home.path(), "user-1", "mcp-session").await.is_none());
        assert!(
            scoped(home.path(), "anonymous", "session-1")
                .await
                .is_none()
        );
    }

    /// Regression: the five webhook adapters substituted a literal
    /// `"unknown"` when they could not read a sender id, and
    /// `source_acl_for_principal` only rejected `""` / `"anonymous"` — so
    /// every anonymous sender on a channel shared one retrieval scope.
    #[test]
    fn ccr_principal_placeholders_never_yield_a_source_acl() {
        for placeholder in [
            "unknown", "UNKNOWN", "Unknown", "anonymous", "ANONYMOUS", "system", "System", "",
            "   ", " unknown ", "\tsystem\n",
        ] {
            assert!(
                is_placeholder_principal(placeholder),
                "{placeholder:?} must be treated as no principal at all"
            );
            assert_eq!(reply_principal_for_sender(placeholder), "");
            assert!(
                source_acl_for_principal("agent", "line:room-1", placeholder).is_none(),
                "{placeholder:?} must fail closed instead of pooling anonymous senders"
            );
        }
    }

    /// The match is exact (after trimming), never a substring: a real sender
    /// id that merely *contains* a reserved word is still a principal.
    #[test]
    fn ccr_principal_matching_is_exact_not_substring() {
        for real in [
            "unknown-user-42",
            "U_SYSTEM_7",
            "ou_anonymous_xyz",
            "systematic",
        ] {
            assert!(!is_placeholder_principal(real));
            assert_eq!(reply_principal_for_sender(real), real);
            assert!(source_acl_for_principal("agent", "line:room-1", real).is_some());
        }
        let a = source_acl_for_principal("agent", "line:room-1", "user-a").unwrap();
        let b = source_acl_for_principal("agent", "line:room-1", "user-b").unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn removed_route_scrubs_old_handle_even_if_readded() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        let both = "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'lookup'\n";
        let search_only = "[ccr]\nenabled = true\n[[ccr.allowed_sources]]\nserver = 'support-mcp'\ntool = 'search'\n";
        std::fs::write(&config, both).unwrap();
        let first = scoped(home.path(), "user-1", "session-1").await.unwrap();
        let search_key = first
            .source_key_for_call(Some("support-mcp"), "search")
            .unwrap();
        let lookup_key = first
            .source_key_for_call(Some("support-mcp"), "lookup")
            .unwrap();
        let kept = first
            .store
            .put(&first.scope, &search_key, "search-call", "search original")
            .unwrap();
        let removed = first
            .store
            .put(&first.scope, &lookup_key, "lookup-call", "lookup original")
            .unwrap();
        std::fs::write(&config, search_only).unwrap();
        let restricted = scoped(home.path(), "user-1", "session-1").await.unwrap();
        assert_eq!(
            restricted.retrieve(&kept.id, None, 0, 64).unwrap().text,
            "search original"
        );
        assert!(matches!(
            restricted
                .store
                .retrieve(&restricted.scope, &removed.id, None, 0, 64),
            Err(duduclaw_llm::CcrError::NotFound)
        ));
        assert!(matches!(
            restricted.store.ensure_source_call_active(
                &restricted.scope,
                &lookup_key,
                "lookup-call"
            ),
            Err(duduclaw_llm::CcrError::Revoked)
        ));
        std::fs::write(&config, both).unwrap();
        let readded = scoped(home.path(), "user-1", "session-1").await.unwrap();
        assert!(matches!(
            readded.retrieve(&removed.id, None, 0, 64),
            Err(duduclaw_llm::CcrError::NotFound)
        ));
        assert!(matches!(
            readded.store.put(
                &readded.scope,
                &lookup_key,
                "lookup-call",
                "lookup original"
            ),
            Err(duduclaw_llm::CcrError::Revoked)
        ));
        // X1 方案 3: with `builtin_sources` defaulting on, "no
        // `[[ccr.allowed_sources]]`" is no longer the same thing as "empty
        // allowlist" — DuDuClaw's own read-only tools are routed. This step is
        // specifically exercising the empty-allowlist denial, so it opts out
        // explicitly rather than relying on a default that moved.
        std::fs::write(
            &config,
            "[ccr]\nenabled = true\nbuiltin_sources = false\n",
        )
        .unwrap();
        assert!(scoped(home.path(), "user-1", "session-1").await.is_none());
        std::fs::write(&config, both).unwrap();
        let restored = scoped(home.path(), "user-1", "session-1").await.unwrap();
        assert!(matches!(
            restored.retrieve(&kept.id, None, 0, 64),
            Err(duduclaw_llm::CcrError::NotFound)
        ));
        let later = restored
            .store
            .put(
                &restored.scope,
                &search_key,
                "search-call-2",
                "later original",
            )
            .unwrap();
        std::fs::write(&config, both.replace("enabled = true", "enabled = false")).unwrap();
        assert!(scoped(home.path(), "user-1", "session-1").await.is_none());
        std::fs::write(&config, both).unwrap();
        let enabled_again = scoped(home.path(), "user-1", "session-1").await.unwrap();
        assert!(matches!(
            enabled_again.retrieve(&later.id, None, 0, 64),
            Err(duduclaw_llm::CcrError::NotFound)
        ));
    }

    /// Regression (W3-2 #1): `capture_delivery_guards` runs once per tool-loop
    /// outcome and each guard opens SQLite, so revalidation must NOT happen
    /// inline on the reactor. The guard here only reports "valid" if another
    /// task on the *same* current-thread runtime got to run while it was
    /// blocking — impossible unless the check was offloaded. Deterministic in
    /// both directions: the inline form fails the recv timeout (false), it
    /// never hangs the suite.
    #[tokio::test]
    async fn capture_delivery_guards_revalidates_off_the_reactor() {
        #[derive(Debug)]
        struct ReactorProbeLease {
            woken: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl CcrDeliveryLease for ReactorProbeLease {
            fn still_valid(&self) -> bool {
                self.woken
                    .lock()
                    .expect("probe receiver")
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .is_ok()
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        tokio::spawn(async move {
            let _ = tx.send(());
        });
        let mut guards = duduclaw_llm::CcrDeliveryGuards::default();
        guards.push(Arc::new(ReactorProbeLease {
            woken: std::sync::Mutex::new(rx),
        }));

        let collector = Arc::new(std::sync::Mutex::new(Vec::new()));
        let retained = DELIVERY_GUARD_COLLECTOR
            .scope(collector.clone(), capture_delivery_guards(guards))
            .await;
        assert!(
            retained,
            "the reactor must stay free while delivery guards revalidate"
        );
        // The caller's task-local scope must still be visible after the await.
        assert_eq!(collector.lock().unwrap().len(), 1);
    }

    /// An empty guard set answers without touching the blocking pool and
    /// needs no collector scope at all.
    #[tokio::test]
    async fn capture_delivery_guards_accepts_an_empty_set_outside_any_scope() {
        assert!(capture_delivery_guards(duduclaw_llm::CcrDeliveryGuards::default()).await);
    }
}
