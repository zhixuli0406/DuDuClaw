//! Validates that `wiki_agent_from_ns` resolves the correct agent directory
//! for external vs. internal principals, and that wiki handlers respect
//! namespace isolation end-to-end.

use super::*;
use crate::mcp_namespace::NamespaceContext;
use std::fs;

// ── Local TempDir ─────────────────────────────────────────────────────────
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("duduclaw-wns-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────

fn external_ns(client_id: &str) -> NamespaceContext {
    NamespaceContext {
        write_namespace: format!("external/{client_id}"),
        read_namespaces: vec![format!("external/{client_id}"), "shared/public".to_string()],
    }
}

fn internal_ns(client_id: &str) -> NamespaceContext {
    NamespaceContext {
        write_namespace: format!("internal/{client_id}"),
        read_namespaces: vec![format!("internal/{client_id}"), "shared/public".to_string()],
    }
}

fn create_agent_dir(home: &std::path::Path, name: &str) {
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\nrole = \"main\"\n"),
    )
    .unwrap();
}

// ── WP-7A bug2: internal tools/list honors per-agent capability ──────────
fn internal_principal_named(client_id: &str) -> crate::mcp_auth::Principal {
    crate::mcp_auth::Principal {
        client_id: client_id.into(),
        scopes: std::collections::HashSet::new(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

fn names_of(resp: &serde_json::Value) -> Vec<String> {
    resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Read `tool_calls.jsonl` and return rows matching the given tool name.
fn read_audit_rows(home: &std::path::Path, tool: &str) -> Vec<serde_json::Value> {
    let path = home.join("tool_calls.jsonl");
    if !path.exists() {
        return vec![];
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("tool_name").and_then(|t| t.as_str()) == Some(tool))
        .collect()
}

/// Minimal agent directory — just enough to satisfy
/// `handle_agent_update_soul`'s "agent.toml exists" check.
fn make_minimal_agent(home: &std::path::Path, name: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\n"),
    )
    .unwrap();
}

mod part1;
mod part2;
mod part3;
