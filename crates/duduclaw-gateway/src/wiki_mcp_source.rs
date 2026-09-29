//! Exact-source attestation for the internal `duduclaw/wiki_read` MCP route.
//!
//! The MCP response and model arguments select a page, but never establish its
//! identity or access state. The gateway rereads the caller's own Wiki and
//! live trust store before allowing a source-bound CCR original. This is a
//! local Wiki connector; it does not attest arbitrary external MCP servers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use duduclaw_llm::{
    CcrDeliveryLease, CcrRuntime, CcrScope, CcrSourceArtifact, McpSourceVerifier, ToolRegistry,
    VerifiedMcpSource,
};
use duduclaw_memory::{WikiDeliveryFence, WikiDeliveryLease, WikiStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const CONNECTOR: &str = "wiki_agent";
const SERVER: &str = "duduclaw";
const TOOL: &str = "wiki_read";
const MAX_RAW_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARTIFACT_ID_BYTES: usize = 512;
/// How long a read-and-verify window waits for an in-flight writer.
///
/// Short on purpose: `still_valid()` runs on the reply path, and a reader
/// that cannot take the fence fails closed (the send is refused). A small
/// wait absorbs a concurrent page write without turning it into a spurious
/// "source changed" refusal, and is far below the SQLite busy timeout this
/// path already tolerates.
const READ_FENCE_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedWikiRoute {
    server: String,
    tool: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WikiSourceAuthority {
    home: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveWikiSource {
    raw: String,
    artifact: CcrSourceArtifact,
}

/// Fence generations observed while the source was read and verified.
///
/// W2-B: a delivery no longer pins the fence for the whole turn. It records
/// the generation of the agent's own Wiki root and of the trust home, then
/// re-checks both before the reply is sent. A change during the turn is
/// detected and the send is refused — the same fail-closed outcome as before,
/// reached by detection instead of by blocking every concurrent writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WikiFenceEpochs {
    wiki: u64,
    home: u64,
}

/// Both fences held for one read-and-verify window, released immediately
/// afterwards. Ordered home-then-wiki everywhere so no two holders can take
/// the same pair in opposite orders.
#[derive(Debug)]
struct WikiReadWindow {
    _home: WikiDeliveryLease,
    _wiki: WikiDeliveryLease,
    epochs: WikiFenceEpochs,
}

#[derive(Debug)]
struct WikiBoundDeliveryLease {
    epochs: WikiFenceEpochs,
    authority: WikiSourceAuthority,
    scope: CcrScope,
    path: String,
    artifact: CcrSourceArtifact,
    saved_sha256: String,
}

impl CcrDeliveryLease for WikiBoundDeliveryLease {
    fn still_valid(&self) -> bool {
        let Ok(window) = self.authority.read_window(&self.scope) else {
            return false;
        };
        if window.epochs != self.epochs {
            return false;
        }
        self.authority
            .read_stable_under_window(&self.scope, &self.path, &window)
            .is_ok_and(|source| {
                source.artifact == self.artifact && source.artifact.version == self.saved_sha256
            })
    }
}

impl WikiSourceAuthority {
    pub(crate) fn new(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
        }
    }

    pub(crate) fn valid_bound(
        &self,
        scope: &CcrScope,
        artifact: &CcrSourceArtifact,
        saved_sha256: &str,
    ) -> bool {
        if artifact.connector != CONNECTOR {
            return false;
        }
        let Ok((agent_id, path)) = serde_json::from_str::<(String, String)>(&artifact.artifact_id)
        else {
            return false;
        };
        if agent_id != scope.agent_id {
            return false;
        }
        self.read_stable(scope, &path).is_ok_and(|source| {
            source.artifact == *artifact && source.artifact.version == saved_sha256
        })
    }

    pub(crate) fn acquire_delivery_lease(
        &self,
        scope: &CcrScope,
        artifact: &CcrSourceArtifact,
        saved_sha256: &str,
    ) -> Result<Arc<dyn CcrDeliveryLease>, String> {
        if artifact.connector != CONNECTOR {
            return Err("unsupported Wiki source".into());
        }
        let (agent_id, path) = serde_json::from_str::<(String, String)>(&artifact.artifact_id)
            .map_err(|_| "invalid Wiki source identity")?;
        if agent_id != scope.agent_id {
            return Err("Wiki source belongs to another agent".into());
        }
        // The shared locks cover only the read-and-verify window; the fence
        // generations observed here are what the pre-send check compares.
        let window = self.read_window(scope)?;
        let source = self.read_stable_under_window(scope, &path, &window)?;
        if source.artifact != *artifact || source.artifact.version != saved_sha256 {
            return Err("Wiki source changed before delivery".into());
        }
        let epochs = window.epochs;
        drop(window);
        Ok(Arc::new(WikiBoundDeliveryLease {
            epochs,
            authority: self.clone(),
            scope: scope.clone(),
            path,
            artifact: artifact.clone(),
            saved_sha256: saved_sha256.into(),
        }))
    }

    /// Take both shared fences for one read-and-verify window.
    ///
    /// The scope is validated first: acquiring a fence creates its directory,
    /// so an unvalidated `agent_id` must never reach the fence constructor.
    fn read_window(&self, scope: &CcrScope) -> Result<WikiReadWindow, String> {
        if scope.tenant_id != "local" || !duduclaw_core::is_valid_agent_id(&scope.agent_id) {
            return Err("invalid Wiki source scope".into());
        }
        let home = WikiDeliveryFence::for_trust_home(&self.home)
            .lock_shared_with_timeout(READ_FENCE_WAIT)
            .map_err(|_| "Wiki delivery fence unavailable")?;
        let wiki = WikiDeliveryFence::for_agent_wiki(&self.home, &scope.agent_id)
            .lock_shared_with_timeout(READ_FENCE_WAIT)
            .map_err(|_| "Wiki delivery fence unavailable")?;
        let epochs = WikiFenceEpochs {
            wiki: wiki.epoch(),
            home: home.epoch(),
        };
        Ok(WikiReadWindow {
            _home: home,
            _wiki: wiki,
            epochs,
        })
    }

    fn read_stable(&self, scope: &CcrScope, path: &str) -> Result<LiveWikiSource, String> {
        let window = self.read_window(scope)?;
        self.read_stable_under_window(scope, path, &window)
    }

    fn read_stable_under_window(
        &self,
        scope: &CcrScope,
        path: &str,
        window: &WikiReadWindow,
    ) -> Result<LiveWikiSource, String> {
        let first = self.read_once(scope, path, window.epochs)?;
        let last = self.read_once(scope, path, window.epochs)?;
        if first != last {
            return Err("Wiki page or trust changed during verification".into());
        }
        Ok(first)
    }

    fn read_once(
        &self,
        scope: &CcrScope,
        path: &str,
        fence_epochs: WikiFenceEpochs,
    ) -> Result<LiveWikiSource, String> {
        if scope.tenant_id != "local"
            || !duduclaw_core::is_valid_agent_id(&scope.agent_id)
            || scope.source_acl.trim().is_empty()
            || path.is_empty()
            || path.len() > MAX_ARTIFACT_ID_BYTES
        {
            return Err("invalid Wiki source scope".into());
        }
        let canonical_home = self
            .home
            .canonicalize()
            .map_err(|_| "Wiki home unavailable")?;
        let wiki_dir = self.home.join("agents").join(&scope.agent_id).join("wiki");
        let canonical_wiki = wiki_dir
            .canonicalize()
            .map_err(|_| "Wiki root unavailable")?;
        if canonical_wiki
            != canonical_home
                .join("agents")
                .join(&scope.agent_id)
                .join("wiki")
        {
            return Err("Wiki root is not the caller's own root".into());
        }
        let page_path = wiki_dir.join(path);
        let canonical_page = page_path
            .canonicalize()
            .map_err(|_| "Wiki page unavailable")?;
        if canonical_page != canonical_wiki.join(path) {
            return Err("Wiki page is outside the allowed root or uses a symlink".into());
        }
        let before = std::fs::metadata(&canonical_page).map_err(|_| "Wiki page unavailable")?;
        if !before.is_file() || before.len() > MAX_RAW_BYTES {
            return Err("Wiki page is outside the allowed root or too large".into());
        }
        let page_revision = file_revision(&before)?;
        let (page, raw) = WikiStore::new(wiki_dir)
            .read_page_with_raw(path)
            .map_err(|_| "Wiki page cannot be read")?;
        let after = std::fs::metadata(&canonical_page).map_err(|_| "Wiki page unavailable")?;
        if !after.is_file()
            || file_revision(&after)? != page_revision
            || page_path.canonicalize().ok().as_ref() != Some(&canonical_page)
        {
            return Err("Wiki page changed during verification".into());
        }
        if raw.is_empty()
            || raw.len() as u64 > MAX_RAW_BYTES
            || !page.trust.is_finite()
            || page.trust < 0.1
            || page.do_not_inject
        {
            return Err("Wiki page is ineligible for CCR".into());
        }
        let trust_revision = self.trust_revision(path, &scope.agent_id)?;
        let artifact_id = serde_json::to_string(&(&scope.agent_id, path))
            .map_err(|_| "invalid Wiki artifact identity")?;
        if artifact_id.len() > MAX_ARTIFACT_ID_BYTES {
            return Err("Wiki artifact identity is too long".into());
        }
        let version = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let acl_revision = format!(
            "wiki-agent-acl-sha256:{:x}",
            Sha256::digest(format!(
                "wiki-agent-v4\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                scope.tenant_id,
                scope.agent_id,
                scope.source_acl,
                trust_revision,
                page_revision,
                fence_epochs.wiki,
                fence_epochs.home
            ))
        );
        Ok(LiveWikiSource {
            raw,
            artifact: CcrSourceArtifact {
                connector: CONNECTOR.into(),
                artifact_id,
                version,
                acl_revision,
            },
        })
    }

    fn trust_revision(&self, path: &str, agent_id: &str) -> Result<String, String> {
        let db = self.home.join("wiki_trust.db");
        let expected_db = self
            .home
            .canonicalize()
            .map_err(|_| "Wiki home unavailable")?
            .join("wiki_trust.db");
        let metadata = match std::fs::symlink_metadata(&db) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A missing trust DB cannot carry a durable generation. If
                // binding were allowed here, create+remove could resurrect
                // the original "absent" revision after a quarantine cycle.
                return Err("Wiki trust store unavailable".into());
            }
            Err(_) => return Err("Wiki trust store unavailable".into()),
        };
        if !metadata.file_type().is_file() || db.canonicalize().ok().as_ref() != Some(&expected_db)
        {
            return Err("Wiki trust store is not a regular file".into());
        }
        // The normal WikiTrustStore opener is a writer and may CREATE a file
        // after the existence check. A source validator must never replace a
        // missing authority with a fresh empty database. Read the same live
        // state table through a strictly read-only SQLite handle instead.
        let mut conn = Connection::open_with_flags(
            &db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "Wiki trust store unavailable")?;
        conn.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(|_| "Wiki trust store unavailable")?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|_| "Wiki trust state unavailable")?;
        let marker: String = tx
            .query_row(
                "SELECT value FROM wiki_trust_meta WHERE key='ccr_revision_trigger_v1'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "Wiki trust revision schema unavailable")?;
        if marker != "1" {
            return Err("Wiki trust revision schema unavailable".into());
        }
        let incarnation: String = tx
            .query_row(
                "SELECT value FROM wiki_trust_meta WHERE key='ccr_db_incarnation_v1'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "Wiki trust incarnation unavailable")?;
        if incarnation.len() != 32 || !incarnation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Wiki trust incarnation invalid".into());
        }
        let state: Option<(f64, i64, String)> = tx
            .query_row(
                "SELECT trust,do_not_inject,updated_at FROM wiki_trust_state
                 WHERE page_path=?1 AND agent_id=?2",
                params![path, agent_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| "Wiki trust state unavailable")?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM wiki_trust_ccr_revisions
                 WHERE page_path=?1 AND agent_id=?2",
                params![path, agent_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Wiki trust revision unavailable")?;
        if revision.is_some_and(|revision| revision <= 0) || (state.is_some() && revision.is_none())
        {
            return Err("Wiki trust revision invalid".into());
        }
        tx.commit().map_err(|_| "Wiki trust read failed")?;
        let current_metadata =
            std::fs::symlink_metadata(&db).map_err(|_| "Wiki trust store disappeared")?;
        if !current_metadata.file_type().is_file()
            || !same_file_identity(&metadata, &current_metadata)
            || db.canonicalize().ok().as_ref() != Some(&expected_db)
        {
            return Err("Wiki trust store changed during verification".into());
        }
        match state {
            Some((trust, do_not_inject, updated_at)) => {
                if !trust.is_finite()
                    || !(0.1..=1.0).contains(&trust)
                    || do_not_inject != 0
                    || updated_at.is_empty()
                    || updated_at.len() > 128
                {
                    return Err("Wiki page is quarantined".into());
                }
                Ok(format!(
                    "db:{incarnation};present:{}:{}:{}:{}",
                    revision.expect("checked state revision"),
                    trust.to_bits(),
                    do_not_inject,
                    updated_at
                ))
            }
            None => Ok(format!(
                "db:{incarnation};no-row:{}",
                revision.map_or_else(|| "never".into(), |revision| revision.to_string())
            )),
        }
    }
}

#[cfg(unix)]
fn file_revision(metadata: &std::fs::Metadata) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!(
        "{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(not(unix))]
fn file_revision(metadata: &std::fs::Metadata) -> Result<String, String> {
    let changed = metadata
        .modified()
        .map_err(|_| "Wiki page timestamp unavailable")?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Wiki page timestamp invalid")?;
    Ok(format!("{}:{}", metadata.len(), changed.as_nanos()))
}

#[cfg(unix)]
fn same_file_identity(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev() && before.ino() == after.ino()
}

#[cfg(not(unix))]
fn same_file_identity(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[derive(Debug)]
struct WikiMcpSourceVerifier {
    authority: WikiSourceAuthority,
}

#[async_trait]
impl McpSourceVerifier for WikiMcpSourceVerifier {
    async fn verify(
        &self,
        scope: &CcrScope,
        args: &Value,
        content: &str,
    ) -> Result<VerifiedMcpSource, String> {
        let args = args
            .as_object()
            .filter(|args| !args.is_empty() && args.len() <= 2)
            .ok_or("invalid Wiki read arguments")?;
        if args
            .keys()
            .any(|key| key != "page_path" && key != "agent_id")
        {
            return Err("unexpected Wiki read argument".into());
        }
        if args
            .get("agent_id")
            .is_some_and(|agent| agent.as_str() != Some(scope.agent_id.as_str()))
        {
            return Err("cross-agent Wiki read cannot be bound".into());
        }
        let path = args
            .get("page_path")
            .and_then(Value::as_str)
            .ok_or("missing Wiki page path")?
            .to_owned();
        let authority = self.authority.clone();
        let scope = scope.clone();
        let exact_content = content.to_owned();
        tokio::task::spawn_blocking(move || {
            let source = authority.read_stable(&scope, &path)?;
            if source.raw != exact_content {
                return Err("MCP result differs from live Wiki page".into());
            }
            Ok(VerifiedMcpSource {
                artifact: source.artifact,
                retention_at: i64::MAX,
            })
        })
        .await
        .map_err(|_| "Wiki verification task failed".to_owned())?
    }
}

/// Only the internal server's own single-page read tool may claim this
/// authority. A declared route withholds output if its source verifier cannot
/// be registered, so it cannot silently become an ordinary MCP result.
pub(crate) fn register_verified_wiki_routes(
    home: &Path,
    registry: &mut ToolRegistry,
    runtime: &CcrRuntime,
) -> Result<(), String> {
    let config = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|error| {
            registry.require_source_attestation_for_all_tools();
            error.to_string()
        })?
        .parse::<toml::Table>()
        .map_err(|error| {
            registry.require_source_attestation_for_all_tools();
            error.to_string()
        })?;
    let routes = config
        .get("ccr")
        .and_then(toml::Value::as_table)
        .and_then(|ccr| ccr.get("verified_wiki_routes"));
    let Some(routes) = routes else {
        registry.disable_ccr_for_tool(TOOL);
        return Ok(());
    };
    registry.require_source_attestation_for_tool(TOOL);
    let routes: Vec<VerifiedWikiRoute> = routes.clone().try_into().map_err(|error| {
        registry.require_source_attestation_for_all_tools();
        format!("invalid ccr.verified_wiki_routes: {error}")
    })?;
    if routes.len() != 1 || routes[0].server != SERVER || routes[0].tool != TOOL {
        return Err("verified Wiki route must be exactly duduclaw/wiki_read".into());
    }
    if runtime.source_key_for_call(Some(SERVER), TOOL).is_none() {
        registry.disable_ccr_for_tool(TOOL);
        return Ok(());
    }
    let verifier = Arc::new(WikiMcpSourceVerifier {
        authority: WikiSourceAuthority::new(home),
    });
    if let Err(error) =
        registry.register_source_verifier(runtime.scope.clone(), SERVER, TOOL, verifier)
    {
        registry.disable_ccr_for_tool(TOOL);
        tracing::warn!(error = %error, "verified Wiki route registration failed; CCR disabled for tool");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use duduclaw_llm::{
        CcrStore, ChatProvider, ChatRequest, ChatResponse, ContentPart, LlmError, McpClient,
        NormalizedUsage, ProvenanceConfig, StopReason, StreamEvent, ToolExecutor,
        run_tool_loop_with_provenance_and_ccr,
    };
    use duduclaw_memory::WikiTrustStore;
    use futures_util::stream::BoxStream;
    use std::sync::Mutex;

    const PAGE_PATH: &str = "concepts/support.md";

    fn scope(agent: &str) -> CcrScope {
        CcrScope {
            tenant_id: "local".into(),
            agent_id: agent.into(),
            session_id: "session".into(),
            source_acl: format!("agent:{agent}:session:session:principal-sha256:test"),
        }
    }

    fn page(body: &str) -> String {
        format!(
            "---\ntitle: Support\ncreated: 2026-01-01\nupdated: 2026-01-01\nlayer: context\ntrust: 0.8\n---\n{body}\n"
        )
    }

    fn write_page(home: &Path, agent: &str, raw: &str) {
        WikiTrustStore::open(&home.join("wiki_trust.db")).unwrap();
        let root = home.join("agents").join(agent).join("wiki/concepts");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("support.md"), raw).unwrap();
    }

    #[tokio::test]
    async fn wiki_verifier_requires_exact_owner_bytes_and_live_trust() {
        let home = tempfile::tempdir().unwrap();
        let raw = page(&"support evidence\n".repeat(400));
        write_page(home.path(), "agent-a", &raw);
        write_page(home.path(), "agent-b", &raw);
        let verifier = WikiMcpSourceVerifier {
            authority: WikiSourceAuthority::new(home.path()),
        };
        let args = serde_json::json!({"page_path": PAGE_PATH});
        let bound = verifier
            .verify(&scope("agent-a"), &args, &raw)
            .await
            .unwrap();
        assert_eq!(bound.artifact.connector, "wiki_agent");
        assert_eq!(
            bound.artifact.version,
            format!("{:x}", Sha256::digest(raw.as_bytes()))
        );
        assert_eq!(bound.retention_at, i64::MAX);
        assert!(verifier.authority.valid_bound(
            &scope("agent-a"),
            &bound.artifact,
            &bound.artifact.version
        ));
        for bad_args in [
            serde_json::json!({"page_path": PAGE_PATH, "agent_id": "agent-b"}),
            serde_json::json!({"page_path": PAGE_PATH, "acl": "owner"}),
            serde_json::json!({"page_path": "../private.md"}),
        ] {
            assert!(
                verifier
                    .verify(&scope("agent-a"), &bad_args, &raw)
                    .await
                    .is_err()
            );
        }
        assert!(
            verifier
                .verify(&scope("agent-a"), &args, "altered")
                .await
                .is_err()
        );
        assert!(!verifier.authority.valid_bound(
            &scope("agent-b"),
            &bound.artifact,
            &bound.artifact.version
        ));

        let trust_db = home.path().join("wiki_trust.db");
        let trust = WikiTrustStore::open(&trust_db).unwrap();
        trust
            .manual_set(
                PAGE_PATH,
                "agent-a",
                0.05,
                false,
                Some(true),
                Some("quarantine"),
            )
            .unwrap();
        drop(trust);
        assert!(
            verifier
                .verify(&scope("agent-a"), &args, &raw)
                .await
                .is_err()
        );
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &bound.artifact,
            &bound.artifact.version
        ));
        std::fs::remove_file(&trust_db).unwrap();
        std::fs::create_dir(&trust_db).unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &bound.artifact,
            &bound.artifact.version
        ));
        std::fs::remove_dir(&trust_db).unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &bound.artifact,
            &bound.artifact.version
        ));
        std::fs::write(
            home.path().join("agents/agent-a/wiki/concepts/support.md"),
            page("updated evidence"),
        )
        .unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &bound.artifact,
            &bound.artifact.version
        ));
    }

    /// W2-B rewrote the semantics this test locks in. The delivery lease no
    /// longer pins the fence for the whole turn, so the page and trust writes
    /// it used to reject now succeed; the lease detects them and refuses the
    /// send instead. Fail-closed is unchanged — only who waits changed.
    #[tokio::test]
    async fn page_write_during_delivery_now_succeeds_and_invalidates_the_lease() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("leased source evidence");
        write_page(home.path(), "agent-a", &raw);
        let authority = WikiSourceAuthority::new(home.path());
        let source = authority.read_stable(&scope("agent-a"), PAGE_PATH).unwrap();
        let lease = authority
            .acquire_delivery_lease(
                &scope("agent-a"),
                &source.artifact,
                &source.artifact.version,
            )
            .unwrap();
        assert!(lease.still_valid());
        let store = WikiStore::new(home.path().join("agents/agent-a/wiki"));
        store.write_page(PAGE_PATH, &page("new evidence")).unwrap();
        assert!(!lease.still_valid());
        assert!(!authority.valid_bound(
            &scope("agent-a"),
            &source.artifact,
            &source.artifact.version
        ));
    }

    /// Same, for the live trust store: the quarantine write is no longer
    /// rejected, and the pending delivery is refused instead.
    #[tokio::test]
    async fn trust_write_during_delivery_now_succeeds_and_invalidates_the_lease() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("leased source evidence");
        write_page(home.path(), "agent-a", &raw);
        let trust = WikiTrustStore::open(home.path().join("wiki_trust.db")).unwrap();
        let authority = WikiSourceAuthority::new(home.path());
        let source = authority.read_stable(&scope("agent-a"), PAGE_PATH).unwrap();
        let lease = authority
            .acquire_delivery_lease(
                &scope("agent-a"),
                &source.artifact,
                &source.artifact.version,
            )
            .unwrap();
        assert!(lease.still_valid());
        trust
            .manual_set(
                PAGE_PATH,
                "agent-a",
                0.05,
                false,
                Some(true),
                Some("quarantine"),
            )
            .unwrap();
        assert!(!lease.still_valid());
    }

    /// Regression (W2-B/a): agent B's page and trust writes must neither be
    /// blocked by nor invalidate agent A's in-flight delivery.
    #[tokio::test]
    async fn another_agents_writes_neither_block_nor_invalidate_a_delivery() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("leased source evidence");
        write_page(home.path(), "agent-a", &raw);
        write_page(home.path(), "agent-b", &raw);
        let trust = WikiTrustStore::open(home.path().join("wiki_trust.db")).unwrap();
        let authority = WikiSourceAuthority::new(home.path());
        let source = authority.read_stable(&scope("agent-a"), PAGE_PATH).unwrap();
        let lease = authority
            .acquire_delivery_lease(
                &scope("agent-a"),
                &source.artifact,
                &source.artifact.version,
            )
            .unwrap();

        WikiStore::new(home.path().join("agents/agent-b/wiki"))
            .write_page(PAGE_PATH, &page("agent b evidence"))
            .unwrap();
        trust
            .manual_set(PAGE_PATH, "agent-b", 0.05, false, Some(true), None)
            .unwrap();

        assert!(lease.still_valid());
        assert!(authority.valid_bound(
            &scope("agent-a"),
            &source.artifact,
            &source.artifact.version
        ));
    }

    /// Regression (W2-B/c): a same-byte rewrite carries the same digests, so
    /// only the per-directory fence generation can catch it.
    #[tokio::test]
    async fn same_byte_rewrite_during_delivery_still_refuses_the_send() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("leased source evidence");
        write_page(home.path(), "agent-a", &raw);
        let authority = WikiSourceAuthority::new(home.path());
        let source = authority.read_stable(&scope("agent-a"), PAGE_PATH).unwrap();
        let lease = authority
            .acquire_delivery_lease(
                &scope("agent-a"),
                &source.artifact,
                &source.artifact.version,
            )
            .unwrap();
        WikiStore::new(home.path().join("agents/agent-a/wiki"))
            .write_page(PAGE_PATH, &raw)
            .unwrap();
        assert!(!lease.still_valid());
    }

    #[tokio::test]
    async fn same_second_trust_quarantine_restore_and_delete_reinsert_never_revive_handle() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("source with a stable byte digest");
        write_page(home.path(), "agent-a", &raw);
        let db = home.path().join("wiki_trust.db");
        let trust = WikiTrustStore::open(&db).unwrap();
        trust
            .manual_set(
                PAGE_PATH,
                "agent-a",
                0.8,
                false,
                Some(false),
                Some("initial"),
            )
            .unwrap();
        drop(trust);
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE wiki_trust_state SET updated_at='2026-01-01 00:00:00'
             WHERE page_path=?1 AND agent_id='agent-a'",
            [PAGE_PATH],
        )
        .unwrap();
        drop(conn);
        let verifier = WikiMcpSourceVerifier {
            authority: WikiSourceAuthority::new(home.path()),
        };
        let args = serde_json::json!({"page_path": PAGE_PATH});
        let first = verifier
            .verify(&scope("agent-a"), &args, &raw)
            .await
            .unwrap();

        // Both changes use the same timestamp and return to the identical
        // trust value, quarantine flag and original page bytes. The durable
        // trigger generation is what prevents an old CCR handle from reviving.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "UPDATE wiki_trust_state SET trust=0.05,do_not_inject=1,
                updated_at='2026-01-01 00:00:00'
              WHERE page_path='concepts/support.md' AND agent_id='agent-a';
             UPDATE wiki_trust_state SET trust=0.8,do_not_inject=0,
                updated_at='2026-01-01 00:00:00'
              WHERE page_path='concepts/support.md' AND agent_id='agent-a';",
        )
        .unwrap();
        drop(conn);
        let restored = verifier
            .verify(&scope("agent-a"), &args, &raw)
            .await
            .unwrap();
        assert_eq!(restored.artifact.version, first.artifact.version);
        assert_ne!(restored.artifact.acl_revision, first.artifact.acl_revision);
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &first.artifact,
            &first.artifact.version
        ));

        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DELETE FROM wiki_trust_state
              WHERE page_path='concepts/support.md' AND agent_id='agent-a';
             INSERT INTO wiki_trust_state(page_path,agent_id,trust,do_not_inject,updated_at)
              VALUES('concepts/support.md','agent-a',0.8,0,'2026-01-01 00:00:00');",
        )
        .unwrap();
        drop(conn);
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &restored.artifact,
            &restored.artifact.version
        ));
    }

    #[tokio::test]
    async fn trust_database_and_page_recreation_do_not_revive_old_binding() {
        let home = tempfile::tempdir().unwrap();
        let raw = page("same bytes across recreation");
        write_page(home.path(), "agent-a", &raw);
        let verifier = WikiMcpSourceVerifier {
            authority: WikiSourceAuthority::new(home.path()),
        };
        let args = serde_json::json!({"page_path": PAGE_PATH});
        let original = verifier
            .verify(&scope("agent-a"), &args, &raw)
            .await
            .unwrap();
        let db = home.path().join("wiki_trust.db");
        std::fs::remove_file(&db).unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &original.artifact,
            &original.artifact.version
        ));
        WikiTrustStore::open(&db).unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &original.artifact,
            &original.artifact.version
        ));
        let rebound = verifier
            .verify(&scope("agent-a"), &args, &raw)
            .await
            .unwrap();
        assert_ne!(
            rebound.artifact.acl_revision,
            original.artifact.acl_revision
        );
        let page_path = home.path().join("agents/agent-a/wiki/concepts/support.md");
        let replacement = home
            .path()
            .join("agents/agent-a/wiki/concepts/replacement.md");
        std::fs::write(&replacement, &raw).unwrap();
        std::fs::rename(replacement, page_path).unwrap();
        assert!(!verifier.authority.valid_bound(
            &scope("agent-a"),
            &rebound.artifact,
            &rebound.artifact.version
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wiki_verifier_rejects_symlinked_root_and_page() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let raw = page("source");
        write_page(home.path(), "agent-a", &raw);
        let verifier = WikiMcpSourceVerifier {
            authority: WikiSourceAuthority::new(home.path()),
        };
        let page_path = home.path().join("agents/agent-a/wiki/concepts/support.md");
        std::fs::write(outside.path().join("support.md"), &raw).unwrap();
        std::fs::remove_file(&page_path).unwrap();
        symlink(outside.path().join("support.md"), &page_path).unwrap();
        assert!(
            verifier
                .verify(
                    &scope("agent-a"),
                    &serde_json::json!({"page_path": PAGE_PATH}),
                    &raw
                )
                .await
                .is_err()
        );
        let wiki_root = home.path().join("agents/agent-a/wiki");
        std::fs::remove_dir_all(&wiki_root).unwrap();
        symlink(outside.path(), &wiki_root).unwrap();
        assert!(
            verifier
                .verify(
                    &scope("agent-a"),
                    &serde_json::json!({"page_path": "support.md"}),
                    &raw
                )
                .await
                .is_err()
        );
    }

    struct OneWikiCall {
        calls: std::sync::atomic::AtomicUsize,
        last_request: Mutex<Option<ChatRequest>>,
    }

    #[async_trait]
    impl ChatProvider for OneWikiCall {
        fn id(&self) -> &str {
            "synthetic"
        }

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, LlmError> {
            self.last_request.lock().unwrap().replace(request.clone());
            let first = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            Ok(ChatResponse {
                parts: if first {
                    vec![ContentPart::ToolCall {
                        id: "wiki-call-1".into(),
                        name: TOOL.into(),
                        args: serde_json::json!({"page_path": PAGE_PATH}),
                    }]
                } else {
                    vec![ContentPart::Text("done".into())]
                },
                stop: if first {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
                usage: NormalizedUsage::default(),
                model_used: "synthetic".into(),
                provider: "synthetic".into(),
            })
        }

        async fn stream(
            &self,
            _request: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::InvalidRequest("stream unused".into()))
        }
    }

    async fn synthetic_mcp_rpc(State(raw): State<String>, Json(frame): Json<Value>) -> Json<Value> {
        let result = match frame["method"].as_str() {
            Some("tools/list") => serde_json::json!({
                "tools": [{"name": TOOL, "inputSchema": {
                    "type": "object", "properties": {"page_path": {"type": "string"}},
                    "required": ["page_path"]
                }}]
            }),
            Some("tools/call") => serde_json::json!({
                "content": [{"type": "text", "text": raw}],
                "isError": false,
                "_meta": {"sourceArtifact": {"connector": "spoofed", "version": "spoofed"}}
            }),
            _ => serde_json::json!({}),
        };
        Json(serde_json::json!({"jsonrpc":"2.0", "id": frame["id"], "result": result}))
    }

    #[tokio::test]
    async fn configured_internal_mcp_wiki_route_commits_and_revalidates_bound_ccr() {
        let home = tempfile::tempdir().unwrap();
        let raw = page(&"trusted Wiki line\n".repeat(400));
        write_page(home.path(), "agent-a", &raw);
        std::fs::write(home.path().join("config.toml"),
            "[ccr]\nenabled=true\nmin_compress_bytes=1024\n[[ccr.allowed_sources]]\nserver='duduclaw'\ntool='wiki_read'\n[[ccr.verified_wiki_routes]]\nserver='duduclaw'\ntool='wiki_read'\n").unwrap();
        let runtime = crate::claude_runner::CHANNEL_REPLY_USER_ID
            .scope("user-a".to_owned(), async {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some("session".to_owned()), async {
                        crate::ccr_runtime::for_agent(home.path(), "agent-a").unwrap()
                    })
                    .await
            })
            .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(synthetic_mcp_rpc))
            .with_state(raw.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = McpClient::connect_http(&endpoint, &[], std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let mut registry =
            ToolRegistry::from_clients_named(vec![(SERVER.into(), client)], Vec::new())
                .await
                .unwrap();
        register_verified_wiki_routes(home.path(), &mut registry, &runtime).unwrap();
        let provider = OneWikiCall {
            calls: std::sync::atomic::AtomicUsize::new(0),
            last_request: Mutex::new(None),
        };
        let outcome = run_tool_loop_with_provenance_and_ccr(
            &provider,
            ChatRequest::new("synthetic"),
            &registry,
            3,
            ProvenanceConfig::default(),
            None,
            Some(runtime.clone()),
        )
        .await
        .unwrap();
        let saved = outcome
            .ccr_saved_results
            .first()
            .expect("bound Wiki CCR handle");
        let seen = provider.last_request.lock().unwrap().clone().unwrap();
        let ContentPart::ToolResult {
            content, is_error, ..
        } = &seen.messages.last().unwrap().parts[0]
        else {
            panic!("missing Wiki result")
        };
        assert!(!is_error);
        assert!(content.contains("id="));
        assert!(content.len() < raw.len());
        assert!(
            runtime
                .retrieve(&saved.id, None, 0, 100)
                .unwrap()
                .text
                .starts_with("---")
        );
        assert!(!runtime.find("trusted Wiki", 5).unwrap().is_empty());

        std::fs::write(
            home.path().join("agents/agent-a/wiki/concepts/support.md"),
            page("changed"),
        )
        .unwrap();
        assert!(runtime.retrieve(&saved.id, None, 0, 100).is_err());
        assert!(runtime.find("trusted Wiki", 5).unwrap().is_empty());
        assert!(
            !runtime
                .valid_saved_reference(
                    &runtime.scope,
                    &saved.id,
                    &saved.source_tool,
                    &saved.source_call_id,
                    saved.original_bytes,
                )
                .unwrap()
        );
        server.abort();
    }

    #[tokio::test]
    async fn absent_or_misconfigured_wiki_route_never_falls_back_to_unbound_ccr() {
        let home = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(synthetic_mcp_rpc))
            .with_state("unverified Wiki bytes".to_owned());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = McpClient::connect_http(&endpoint, &[], std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let mut registry =
            ToolRegistry::from_clients_named(vec![(SERVER.into(), client)], Vec::new())
                .await
                .unwrap();
        let runtime = CcrRuntime::new(CcrStore::new(home.path().join("ccr.db")), scope("agent-a"))
            .restrict_sources([(SERVER.into(), TOOL.into())]);
        std::fs::write(home.path().join("config.toml"), "[ccr]\nenabled=true\n").unwrap();
        register_verified_wiki_routes(home.path(), &mut registry, &runtime).unwrap();
        assert!(
            !registry
                .call(TOOL, serde_json::json!({"page_path": PAGE_PATH}))
                .await
                .unwrap()
                .ccr_eligible
        );
        std::fs::write(
            home.path().join("config.toml"),
            "[ccr]\n[[ccr.verified_wiki_routes]]\nserver='other'\ntool='wiki_read'\n",
        )
        .unwrap();
        assert!(register_verified_wiki_routes(home.path(), &mut registry, &runtime).is_err());
        let refused = registry
            .call(TOOL, serde_json::json!({"page_path": PAGE_PATH}))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.ccr_revoke_call);
        assert!(!refused.content.contains("unverified Wiki bytes"));
        server.abort();
    }
}
