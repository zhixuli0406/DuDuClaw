//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── (removed) P2 GOV helpers ──────────────────────────────────────────────────
//
// G2, 2026-09 feature audit: the Governance Layer's `policies/*.yaml`
// emitter/parser/validator lived here. Its enforcer — the `duduclaw-governance`
// crate — was deleted in `b0639b96`, so writing a policy changed nothing at
// runtime; rate limiting is `mcp_rate_limit`, permissions are
// `delegation_policy` + the MCP scope table, and quotas are `license_runtime`.
// The dashboard page, the three RPCs and `docs/features/21` went with it.

// ── P2 SCP helpers (.scope.toml wiki namespace policy) ────────────────────────
//
// Mirrors `duduclaw-cli::wiki_scope` (`[namespaces."<ns>"] mode = "..."`).
// Path: `<home>/shared/wiki/.scope.toml`.

/// Valid namespace modes (mirror `wiki_scope::NamespaceMode`).
pub(crate) const SCP_MODES: &[&str] = duduclaw_core::scope_policy::MODES;

/// Convert a parsed `.scope.toml` table into the `wiki_scope.get` response:
/// `{ namespaces: [{ namespace, mode, synced_from }] }`.
pub(crate) fn scp_table_to_response(table: &toml::Table) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Some(ns) = table.get("namespaces").and_then(|v| v.as_table()) {
        for (name, entry) in ns {
            let t = match entry.as_table() {
                Some(t) => t,
                None => continue,
            };
            let mode = t
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("agent_writable");
            let synced_from = t.get("synced_from").and_then(|v| v.as_str());
            out.push(json!({
                "namespace": name,
                "mode": mode,
                "synced_from": synced_from,
                "agents": t.get("agents").and_then(|v| v.as_array()),
            }));
        }
    }
    out.sort_by(|a, b| {
        a["namespace"]
            .as_str()
            .unwrap_or("")
            .cmp(b["namespace"].as_str().unwrap_or(""))
    });
    json!({ "namespaces": out })
}

/// Apply a `wiki_scope.update` payload onto a `.scope.toml` table. Sets (or, on
/// `mode == "agent_writable"` with `remove == true`, deletes) a single
/// namespace's policy. Returns the change description. Validates the mode enum
/// + that `read_only` carries a non-empty `synced_from`.
pub(crate) fn scp_apply_namespace(
    table: &mut toml::Table,
    namespace: &str,
    mode: &str,
    synced_from: Option<&str>,
    remove: bool,
) -> Result<String, String> {
    if namespace.is_empty() || namespace.contains('/') || namespace.len() > 128 {
        return Err("namespace must be a non-empty top-level segment (no '/'), ≤128 chars".into());
    }
    let ns_table = table
        .entry("namespaces")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("Invalid [namespaces] section")?;

    if remove {
        ns_table.remove(namespace);
        return Ok(format!(
            "namespace '{namespace}' policy removed (defaults to agent_writable)"
        ));
    }

    if !SCP_MODES.contains(&mode) {
        return Err(format!(
            "Invalid mode '{mode}'. Valid: {}",
            SCP_MODES.join(", ")
        ));
    }
    let mut entry = ns_table.get(namespace).and_then(|v| v.as_table()).cloned().unwrap_or_default();
    entry.remove("synced_from");
    if mode != "agent_allowlist" { entry.remove("agents"); }
    entry.insert("mode".into(), toml::Value::String(mode.into()));
    if mode == "read_only" {
        let sf = synced_from.map(str::trim).unwrap_or("");
        if sf.is_empty() {
            return Err("mode 'read_only' requires a non-empty 'synced_from'".into());
        }
        entry.insert("synced_from".into(), toml::Value::String(sf.into()));
    }
    ns_table.insert(namespace.into(), toml::Value::Table(entry));
    Ok(format!("namespace '{namespace}' = {mode}"))
}

/// Look up the declared mode for a single namespace in a parsed `.scope.toml`
/// table. Returns `None` when the namespace has no explicit policy (defaults to
/// `agent_writable`).
pub(crate) fn scp_namespace_mode(table: &toml::Table, namespace: &str) -> Option<String> {
    table
        .get("namespaces")?
        .as_table()?
        .get(namespace)?
        .as_table()?
        .get("mode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// W2-5: strict `.scope.toml` parse for the `wiki_scope.get`/`wiki_scope.update`
/// RPC pair. Blank/absent content is the canonical "no policy configured"
/// state (→ empty table, unchanged behavior); non-blank content that fails to
/// parse is `Err` and MUST NOT be written back over.
///
/// This exists because the generic `read_config_table` (shared by every other
/// RPC in this file) silently downgrades a parse failure to an empty table —
/// exactly the wrong behavior for `wiki_scope.update` specifically: that
/// handler previously called `read_config_table`, got back an "empty" table
/// for a merely-malformed file (e.g. one unbalanced quote from a manual edit),
/// applied the requested change to that empty table, and wrote it back —
/// silently erasing every OTHER operator namespace declaration the file
/// actually still held. Fail-closed here means refusing the write entirely
/// and leaving the on-disk file untouched (CLAUDE.md convention 4).
pub(crate) fn parse_scp_table_strict(content: &str) -> Result<toml::Table, String> {
    duduclaw_core::scope_policy::ScopePolicy::parse(content)
        .map(|policy| policy.table)
        .map_err(|e| format!(".scope.toml is malformed and was left untouched: {e}"))
}

pub(crate) fn load_scp_table(path: &Path) -> Result<toml::Table, String> {
    duduclaw_core::scope_policy::load(path).map(|policy| policy.table).map_err(|e| e.to_string())
}

#[cfg(test)]
#[test]
fn rm209_dashboard_editor_policy_matrix() {
    let home = tempfile::tempdir().unwrap();
    let wiki = home.path().join("shared/wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    let path = wiki.join(".scope.toml");
    let allowed = || { load_scp_table(&path).is_ok() };
    assert!(allowed(), "missing file retains no-policy behavior");
    for raw in ["not [ valid toml", "namespaces = 42", "[namespaces.notes]\nvisible_to_departments = [7]", "[namespaces.notes]\nmode = false"] {
        std::fs::write(&path, raw).unwrap();
        assert!(!allowed(), "malformed policy must deny: {raw}");
    }
    std::fs::write(&path, "").unwrap();
    assert!(allowed(), "empty valid policy");
    std::fs::remove_file(&path).unwrap();
    let target = home.path().join("target.toml");
    std::fs::write(&target, "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &path).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &path) {
        eprintln!("RM-209: skipping link cases: Windows symlink permission unavailable: {error}");
        return;
    }
    #[cfg(any(unix, windows))]
    {
        assert!(!allowed(), "valid symlink must deny");
        std::fs::remove_file(&target).unwrap();
        assert!(!allowed(), "dangling symlink must deny");
    }
}

#[cfg(test)]
#[test]
fn rm209_editor_allowlist_and_visibility_round_trip() {
    let mut table = parse_scp_table_strict("[namespaces.hr]\nvisible_to_departments = ['hr']\nagents = ['alice']\nmode = 'agent_allowlist'").unwrap();
    assert!(SCP_MODES.contains(&"agent_allowlist"));
    scp_apply_namespace(&mut table, "hr", "agent_allowlist", None, false).unwrap();
    let response = scp_table_to_response(&table);
    assert_eq!(response["namespaces"][0]["agents"][0], "alice");
    assert_eq!(table["namespaces"]["hr"]["visible_to_departments"][0].as_str(), Some("hr"));
}
