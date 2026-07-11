//! wiki_scope.rs — RFC-21 §3: Shared wiki Source-of-Truth namespace policy.
//!
//! Loads `~/.duduclaw/shared/wiki/.scope.toml` and decides whether a write to
//! `shared_wiki_write` is allowed for a given top-level namespace.
//!
//! ## Policy file format
//!
//! ```toml
//! [namespaces."identity"]
//! mode         = "read_only"
//! synced_from  = "identity-provider"
//!
//! [namespaces."access"]
//! mode         = "read_only"
//! synced_from  = "policy-registry"
//!
//! [namespaces."SOP"]
//! mode         = "agent_writable"      # explicit (also the default)
//!
//! [namespaces."policies"]
//! mode         = "operator_only"
//! ```
//!
//! ## Defaults / fail-safe
//!
//! - File absent       → every namespace `AgentWritable` (no semantic change vs. v1.10.1).
//! - File malformed    → log warning, treat as absent (fail-safe; never blocks the gateway).
//! - Namespace absent  → `AgentWritable` (only listed namespaces tighten).
//!
//! ## Hot-reload
//!
//! The policy is re-read from disk on every call to [`load_for`]. The file is
//! tiny (a few KB at most) and shared-wiki writes are not on the hot path, so
//! we trade one `std::fs::read_to_string` per write for zero-cost
//! "hot-reload" — operators may edit `.scope.toml` and the next write picks
//! up the change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Public types ─────────────────────────────────────────────────────────────

/// Namespace write-policy mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceMode {
    /// Default — any agent that passes existing `shared_wiki_write` checks may write.
    AgentWritable,
    /// Only writers whose [`WriterCapability`] matches `synced_from` may write.
    /// All other callers are denied.
    ReadOnly { synced_from: String },
    /// Never writable via the MCP path. Only the operator CLI (`duduclaw wiki sync` /
    /// `duduclaw wiki scope`) may write here, and that surface goes through
    /// [`WriterCapability::Operator`], not through MCP.
    OperatorOnly,
    /// Only agents whose exact id is in `agents` (plus the operator) may write.
    /// This is the WP3 answer to "誰可以寫入" — the operator lists which agents
    /// own a namespace. An **empty** list is fail-closed: it denies every agent
    /// (equivalent to `operator_only`), never "open to all".
    AgentAllowlist { agents: Vec<String> },
}

impl NamespaceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            NamespaceMode::AgentWritable => "agent_writable",
            NamespaceMode::ReadOnly { .. } => "read_only",
            NamespaceMode::OperatorOnly => "operator_only",
            NamespaceMode::AgentAllowlist { .. } => "agent_allowlist",
        }
    }
}

/// Identifier carried by the writer; determines which `synced_from` and
/// `OperatorOnly` slots they may fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterCapability {
    /// Normal MCP path — caller is the agent named here.
    Mcp { agent_id: String },
    /// Internal capability granted by the gateway to a specific subsystem
    /// (identity provider, policy registry sync, ...). Compared verbatim
    /// against `synced_from`.
    Internal { capability: String },
    /// Operator-side CLI / sync command. Allowed to write `OperatorOnly`
    /// namespaces and any `synced_from` slot (operators have superpowers
    /// because they already have shell access to the wiki dir).
    Operator,
}

impl WriterCapability {
    pub fn for_agent(agent_id: impl Into<String>) -> Self {
        WriterCapability::Mcp { agent_id: agent_id.into() }
    }

    /// Caller-facing label for audit logs / error messages. Never returns
    /// secret material (capability names are non-secret identifiers).
    pub fn label(&self) -> String {
        match self {
            WriterCapability::Mcp { agent_id } => format!("agent:{agent_id}"),
            WriterCapability::Internal { capability } => format!("internal:{capability}"),
            WriterCapability::Operator => "operator".to_string(),
        }
    }
}

/// Reason a write was denied — surfaced to the caller verbatim and into
/// `audit.unified_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDeny {
    pub namespace: String,
    pub mode: String,
    pub reason: String,
}

impl std::fmt::Display for ScopeDeny {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "namespace '{}' is {} — {}",
            self.namespace, self.mode, self.reason
        )
    }
}

/// Loaded namespace-policy table.
#[derive(Debug, Clone, Default)]
pub struct WikiScopePolicy {
    namespaces: BTreeMap<String, NamespaceMode>,
    loaded_from: Option<PathBuf>,
}

impl WikiScopePolicy {
    /// Empty policy: every namespace is `AgentWritable`. This is the
    /// canonical default for deployments without a `.scope.toml` file.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from `<home_dir>/shared/wiki/.scope.toml`, returning an empty
    /// policy on any failure (absent file / malformed TOML / read error).
    /// Failures are logged at WARN level but never propagated.
    pub fn load_for(home_dir: &Path) -> Self {
        let path = scope_file_path(home_dir);
        Self::load_from(&path)
    }

    /// Load from an explicit path. Returns empty on absent/malformed file.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => match parse_toml(&raw) {
                Ok(map) => Self { namespaces: map, loaded_from: Some(path.to_path_buf()) },
                Err(e) => {
                    warn!(
                        "Skipping malformed wiki scope policy at {:?}: {} \
                         (treating as no policy — all namespaces writable)",
                        path, e
                    );
                    Self::empty()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::empty(),
            Err(e) => {
                warn!("Failed to read wiki scope policy at {:?}: {}", path, e);
                Self::empty()
            }
        }
    }

    /// Mode for a top-level namespace. Unlisted namespaces default to
    /// [`NamespaceMode::AgentWritable`].
    pub fn mode_for(&self, namespace: &str) -> NamespaceMode {
        self.namespaces
            .get(namespace)
            .cloned()
            .unwrap_or(NamespaceMode::AgentWritable)
    }

    /// Resolve the namespace from a wiki-relative `page_path` and check
    /// whether `caller` may write to it.
    pub fn check_write(
        &self,
        page_path: &str,
        caller: &WriterCapability,
    ) -> Result<(), ScopeDeny> {
        let namespace = top_level_namespace(page_path);
        let mode = self.mode_for(&namespace);

        match (&mode, caller) {
            // Default and explicit agent_writable: everyone may write
            // (subject to other checks layered on top).
            (NamespaceMode::AgentWritable, _) => Ok(()),

            // Operator may write any namespace.
            (_, WriterCapability::Operator) => Ok(()),

            // Read-only namespace: only writers whose internal capability
            // matches `synced_from` are allowed.
            (NamespaceMode::ReadOnly { synced_from }, WriterCapability::Internal { capability })
                if capability == synced_from =>
            {
                Ok(())
            }
            (NamespaceMode::ReadOnly { synced_from }, _) => Err(ScopeDeny {
                namespace,
                mode: "read_only".into(),
                reason: format!(
                    "writes restricted to internal capability '{synced_from}'; caller is {}",
                    caller.label()
                ),
            }),

            // Agent allowlist: only an MCP caller whose EXACT id is on the list
            // may write (coding convention #2 — exact equality, no contains/
            // prefix). Operator was already admitted above. An empty list
            // matches nobody ⇒ fail-closed (operator-only equivalent).
            (NamespaceMode::AgentAllowlist { agents }, WriterCapability::Mcp { agent_id })
                if agents.iter().any(|a| a == agent_id) =>
            {
                Ok(())
            }
            (NamespaceMode::AgentAllowlist { agents }, _) => Err(ScopeDeny {
                namespace,
                mode: "agent_allowlist".into(),
                reason: if agents.is_empty() {
                    format!(
                        "write allowlist is empty (fail-closed — operator only); caller is {}",
                        caller.label()
                    )
                } else {
                    format!(
                        "writes restricted to agents [{}]; caller is {}",
                        agents.join(", "),
                        caller.label()
                    )
                },
            }),

            // Operator-only namespace: nothing else gets through.
            (NamespaceMode::OperatorOnly, _) => Err(ScopeDeny {
                namespace,
                mode: "operator_only".into(),
                reason: "this namespace is only writable from the operator CLI".into(),
            }),
        }
    }

    /// Render the policy as a JSON-friendly value for `wiki_namespace_status`.
    pub fn snapshot(&self) -> Vec<NamespaceSnapshot> {
        self.namespaces
            .iter()
            .map(|(name, mode)| NamespaceSnapshot {
                namespace: name.clone(),
                mode: mode.as_str().into(),
                synced_from: match mode {
                    NamespaceMode::ReadOnly { synced_from } => Some(synced_from.clone()),
                    _ => None,
                },
                agents: match mode {
                    NamespaceMode::AgentAllowlist { agents } => Some(agents.clone()),
                    _ => None,
                },
            })
            .collect()
    }

    pub fn loaded_from(&self) -> Option<&Path> {
        self.loaded_from.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        self.namespaces.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamespaceSnapshot {
    pub namespace: String,
    pub mode: String,
    pub synced_from: Option<String>,
    /// Populated only for `agent_allowlist` mode: the exact agent ids allowed
    /// to write this namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<String>>,
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// Reserved policy filename — never permitted as a `shared_wiki_write` target.
pub const SCOPE_POLICY_FILENAME: &str = ".scope.toml";

/// Resolves `<home_dir>/shared/wiki/.scope.toml`.
pub fn scope_file_path(home_dir: &Path) -> PathBuf {
    home_dir.join("shared").join("wiki").join(SCOPE_POLICY_FILENAME)
}

/// Extract the top-level namespace from a wiki-relative path. Pages directly
/// at the root (no slash) live in the synthetic `""` namespace, which is
/// always `AgentWritable` unless an operator explicitly lists it.
pub fn top_level_namespace(page_path: &str) -> String {
    match page_path.split('/').next() {
        Some(seg) if !seg.is_empty() && seg != page_path => seg.to_string(),
        // Either no slash at all (root file) or empty leading segment.
        _ => String::new(),
    }
}

// ── TOML parsing (private) ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ScopeFile {
    #[serde(default)]
    namespaces: BTreeMap<String, NamespaceEntry>,
}

#[derive(Debug, Deserialize)]
struct NamespaceEntry {
    mode: String,
    #[serde(default)]
    synced_from: Option<String>,
    #[serde(default)]
    agents: Option<Vec<String>>,
}

fn parse_toml(raw: &str) -> Result<BTreeMap<String, NamespaceMode>, String> {
    let parsed: ScopeFile = toml::from_str(raw).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for (name, entry) in parsed.namespaces {
        let mode = match entry.mode.as_str() {
            "agent_writable" => NamespaceMode::AgentWritable,
            "read_only" => {
                let synced_from = entry.synced_from.unwrap_or_default();
                if synced_from.is_empty() {
                    return Err(format!(
                        "namespace '{name}' has mode = \"read_only\" but no `synced_from`"
                    ));
                }
                NamespaceMode::ReadOnly { synced_from }
            }
            "operator_only" => NamespaceMode::OperatorOnly,
            "agent_allowlist" => {
                // Empty / absent list is allowed and is fail-closed at write
                // time (denies every agent — see `check_write`). We normalise
                // absent → empty here rather than erroring so an operator can
                // stage a namespace as "locked, fill in agents later".
                let agents = entry.agents.unwrap_or_default();
                NamespaceMode::AgentAllowlist { agents }
            }
            other => {
                return Err(format!(
                    "namespace '{name}' has unknown mode '{other}' \
                     (expected 'agent_writable' / 'read_only' / 'operator_only' / 'agent_allowlist')"
                ));
            }
        };
        out.insert(name, mode);
    }
    Ok(out)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_policy(home: &Path, body: &str) {
        let path = scope_file_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    // ── WP3 agent_allowlist mode ──────────────────────────────────

    fn allowlist_policy(agents: &[&str]) -> WikiScopePolicy {
        let list = agents
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let body = format!(
            "[namespaces.policies]\nmode = \"agent_allowlist\"\nagents = [{list}]\n"
        );
        let map = parse_toml(&body).unwrap();
        WikiScopePolicy { namespaces: map, loaded_from: None }
    }

    #[test]
    fn agent_allowlist_admits_listed_agent_exactly() {
        let p = allowlist_policy(&["agnes", "boss"]);
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::for_agent("agnes"))
            .is_ok());
        // Exact match only — a superstring id must NOT pass (convention #2).
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::for_agent("agnes-2"))
            .is_err());
        // Agent not on the list is denied.
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::for_agent("intruder"))
            .is_err());
    }

    #[test]
    fn agent_allowlist_operator_always_allowed() {
        let p = allowlist_policy(&["agnes"]);
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::Operator)
            .is_ok());
    }

    #[test]
    fn agent_allowlist_empty_is_fail_closed() {
        // Empty list denies every agent (operator-only equivalent), never open.
        let p = allowlist_policy(&[]);
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::for_agent("agnes"))
            .is_err());
        assert!(p
            .check_write("policies/hr.md", &WriterCapability::Operator)
            .is_ok());
    }

    #[test]
    fn agent_allowlist_snapshot_carries_agents() {
        let p = allowlist_policy(&["agnes", "boss"]);
        let snap = p.snapshot();
        let ns = snap.iter().find(|s| s.namespace == "policies").unwrap();
        assert_eq!(ns.mode, "agent_allowlist");
        assert_eq!(ns.agents.as_deref(), Some(&["agnes".to_string(), "boss".to_string()][..]));
    }

    #[test]
    fn top_level_namespace_extracts_first_segment() {
        assert_eq!(top_level_namespace("identity/discord-users.md"), "identity");
        assert_eq!(top_level_namespace("access/blocklist.md"), "access");
        assert_eq!(top_level_namespace("a/b/c/d.md"), "a");
        assert_eq!(top_level_namespace("root.md"), "");
        assert_eq!(top_level_namespace(""), "");
        assert_eq!(top_level_namespace("/leading-slash.md"), "");
    }

    #[test]
    fn empty_policy_treats_every_namespace_as_writable() {
        let p = WikiScopePolicy::empty();
        assert_eq!(p.mode_for("identity"), NamespaceMode::AgentWritable);
        assert_eq!(p.mode_for("anything"), NamespaceMode::AgentWritable);
        let caller = WriterCapability::for_agent("agnes");
        assert!(p.check_write("identity/foo.md", &caller).is_ok());
    }

    #[test]
    fn missing_file_yields_empty_policy() {
        let tmp = TempDir::new().unwrap();
        let p = WikiScopePolicy::load_for(tmp.path());
        assert!(p.is_empty());
        assert!(p.loaded_from().is_none());
    }

    #[test]
    fn malformed_toml_yields_empty_policy_and_does_not_panic() {
        let tmp = TempDir::new().unwrap();
        write_policy(tmp.path(), "this is :: not = valid = toml ===");
        let p = WikiScopePolicy::load_for(tmp.path());
        // Fail-safe: never blocks the gateway.
        assert!(p.is_empty());
    }

    #[test]
    fn unknown_mode_value_is_rejected_at_parse_time() {
        let raw = r#"
            [namespaces."identity"]
            mode = "broadcast_to_world"
        "#;
        let err = parse_toml(raw).unwrap_err();
        assert!(err.contains("unknown mode"), "got: {err}");
    }

    #[test]
    fn read_only_without_synced_from_is_rejected() {
        let raw = r#"
            [namespaces."identity"]
            mode = "read_only"
        "#;
        let err = parse_toml(raw).unwrap_err();
        assert!(err.contains("synced_from"), "got: {err}");
    }

    #[test]
    fn read_only_namespace_blocks_arbitrary_agent() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        let caller = WriterCapability::for_agent("agnes");
        let err = p.check_write("identity/discord-users.md", &caller).unwrap_err();
        assert_eq!(err.namespace, "identity");
        assert_eq!(err.mode, "read_only");
        assert!(err.reason.contains("identity-provider"));
        assert!(err.reason.contains("agent:agnes"));
    }

    #[test]
    fn read_only_namespace_allows_matching_internal_capability() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        let caller = WriterCapability::Internal { capability: "identity-provider".into() };
        assert!(p.check_write("identity/discord-users.md", &caller).is_ok());
    }

    #[test]
    fn read_only_namespace_rejects_mismatched_internal_capability() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        let caller = WriterCapability::Internal { capability: "policy-registry".into() };
        assert!(p.check_write("identity/x.md", &caller).is_err());
    }

    #[test]
    fn operator_only_namespace_blocks_every_mcp_caller() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."policies"]
                mode = "operator_only"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());

        for caller in [
            WriterCapability::for_agent("agnes"),
            WriterCapability::Internal { capability: "identity-provider".into() },
        ] {
            let err = p.check_write("policies/security.md", &caller).unwrap_err();
            assert_eq!(err.mode, "operator_only");
        }
    }

    #[test]
    fn operator_capability_overrides_every_restriction() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"

                [namespaces."policies"]
                mode = "operator_only"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        assert!(p.check_write("identity/x.md", &WriterCapability::Operator).is_ok());
        assert!(p.check_write("policies/y.md", &WriterCapability::Operator).is_ok());
    }

    #[test]
    fn unlisted_namespace_falls_through_to_agent_writable() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        let caller = WriterCapability::for_agent("agnes");
        // SOP namespace is not listed → should be writable.
        assert!(p.check_write("SOP/onboarding.md", &caller).is_ok());
        // Root file → also writable.
        assert!(p.check_write("loose-page.md", &caller).is_ok());
    }

    #[test]
    fn snapshot_lists_only_configured_namespaces_in_stable_order() {
        let tmp = TempDir::new().unwrap();
        write_policy(
            tmp.path(),
            r#"
                [namespaces."policies"]
                mode = "operator_only"

                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p = WikiScopePolicy::load_for(tmp.path());
        let snap = p.snapshot();
        // BTreeMap → alphabetical
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].namespace, "identity");
        assert_eq!(snap[0].mode, "read_only");
        assert_eq!(snap[0].synced_from.as_deref(), Some("identity-provider"));
        assert_eq!(snap[1].namespace, "policies");
        assert_eq!(snap[1].mode, "operator_only");
        assert!(snap[1].synced_from.is_none());
    }

    #[test]
    fn writer_capability_label_is_audit_friendly() {
        assert_eq!(WriterCapability::for_agent("agnes").label(), "agent:agnes");
        assert_eq!(
            WriterCapability::Internal { capability: "identity-provider".into() }.label(),
            "internal:identity-provider"
        );
        assert_eq!(WriterCapability::Operator.label(), "operator");
    }

    #[test]
    fn hot_reload_picks_up_edits_on_next_load() {
        let tmp = TempDir::new().unwrap();
        // Round 1: identity is read_only.
        write_policy(
            tmp.path(),
            r#"
                [namespaces."identity"]
                mode = "read_only"
                synced_from = "identity-provider"
            "#,
        );
        let p1 = WikiScopePolicy::load_for(tmp.path());
        let caller = WriterCapability::for_agent("agnes");
        assert!(p1.check_write("identity/x.md", &caller).is_err());

        // Round 2: operator relaxes the policy by editing the file.
        write_policy(tmp.path(), "");
        let p2 = WikiScopePolicy::load_for(tmp.path());
        assert!(p2.is_empty());
        assert!(p2.check_write("identity/x.md", &caller).is_ok());
    }
}
