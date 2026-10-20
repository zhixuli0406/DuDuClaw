//! The MCP sources Grok reads by itself from a working directory, besides
//! the `.grok/config.toml` DuDuClaw writes (2026-10-20).
//!
//! Measured on Grok 1.0.62 with `grok inspect` / `grok mcp doctor` under a
//! temporary HOME, and stated in its bundled docs (`07-mcp-servers.md`
//! "Compatibility", `05-configuration.md`):
//!
//! - `<cwd>/.mcp.json`, and the `.mcp.json` of every directory up to the git
//!   repository root (no git repository: the working directory only);
//! - `<cwd>/.cursor/mcp.json` (Cursor compatibility, `[compat.cursor] mcps`,
//!   on by default);
//! - the `.grok/config.toml` of every directory up to the git root (lower
//!   priority than the working directory's own);
//! - user-level sources outside the employee directory (`~/.grok`,
//!   `~/.claude.json`, `~/.cursor/mcp.json`), not handled here.
//!
//! Not loaded (measured): `<cwd>/.claude.json`, `<cwd>/.claude/settings.json`
//! and `settings.local.json` `mcpServers`.
//!
//! Every source ranks below the working directory's `.grok/config.toml`, so
//! each name found here is shadowed there.

use std::path::{Path, PathBuf};

/// Every server name Grok would load from `dir` by itself, other than from
/// `dir/.mcp.json` (the caller reads that one; for the employee's own
/// directory it comes from the same read the gate used). `Err` when a source
/// exists but cannot be read or parsed: Grok might still read it, so the
/// caller refuses rather than leave a name unshadowed.
pub(crate) fn other_source_names(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = json_server_names(&dir.join(".cursor").join("mcp.json"))?;
    for ancestor in ancestors_to_git_root(dir) {
        names.extend(json_server_names(&ancestor.join(".mcp.json"))?);
        names.extend(json_server_names(
            &ancestor.join(".cursor").join("mcp.json"),
        )?);
        names.extend(toml_server_names(
            &ancestor.join(".grok").join("config.toml"),
        )?);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// The directories strictly above `dir` up to and including the root of the
/// git repository `dir` is in; empty outside a repository (Grok then reads
/// the working directory only).
pub(crate) fn ancestors_to_git_root(dir: &Path) -> Vec<PathBuf> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let Some(root) = canon
        .ancestors()
        .find(|a| std::fs::symlink_metadata(a.join(".git")).is_ok())
    else {
        return Vec::new();
    };
    canon
        .ancestors()
        .skip(1)
        .take_while(|a| a.starts_with(root))
        .map(Path::to_path_buf)
        .collect()
}

/// `mcpServers` keys of a JSON MCP config; a missing file is empty.
pub(crate) fn json_server_names(path: &Path) -> Result<Vec<String>, String> {
    let Some(raw) = read_optional(path)? else {
        return Ok(Vec::new());
    };
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    match v.get("mcpServers") {
        None => Ok(Vec::new()),
        Some(serde_json::Value::Object(m)) => Ok(m.keys().cloned().collect()),
        Some(_) => Err(format!("{}: mcpServers is not an object", path.display())),
    }
}

/// `[mcp_servers]` keys of a Grok TOML config; a missing file is empty.
pub(crate) fn toml_server_names(path: &Path) -> Result<Vec<String>, String> {
    let Some(raw) = read_optional(path)? else {
        return Ok(Vec::new());
    };
    let v: toml::Value =
        toml::from_str(&raw).map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    match v.get("mcp_servers") {
        None => Ok(Vec::new()),
        Some(toml::Value::Table(t)) => Ok(t.keys().cloned().collect()),
        Some(_) => Err(format!("{}: mcp_servers is not a table", path.display())),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} is unreadable: {e}", path.display())),
    }
}
