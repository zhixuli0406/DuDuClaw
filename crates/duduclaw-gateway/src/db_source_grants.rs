//! Per-agent grants for the read-only SQL data sources — the dashboard side of
//! `agent.toml [capabilities] db_sources` (WP-D,
//! `DESIGN-redaction-field-rules-2026-09` §13.7).
//!
//! The four `db_*` MCP tools are deny-by-default: an agent may only reach a
//! `config.toml [db_sources.<id>]` source that its own `[capabilities]
//! db_sources` list names (`duduclaw-cli::mcp_dispatch` §3.628). Until this
//! module existed the only way to grant one was hand-editing `agent.toml`, so
//! the source registry had an operator UI and the *authorization* did not.
//!
//! Two admin RPCs, routed through `db_sources_rpc::dispatch` next to the five
//! that already manage the sources themselves:
//!
//! | method | shape |
//! |---|---|
//! | `db_sources.grants.list` | every configured source × who holds it, every agent, and every **stale** grant |
//! | `db_sources.grants.set` | replace the grant set of ONE source across all agents |
//!
//! ## Three properties this module is built around
//!
//! **Fail-closed naming.** `grants.set` refuses a source id that is not
//! configured and an agent id that has no `agent.toml`, and it refuses them
//! *before* the first write, so a half-applied grant set is not reachable
//! through a typo. The rejection lists the configured ids — ids only, never a
//! connection string (WP-H1 credential doctrine, same rule the sibling
//! `db_sources.*` RPCs follow).
//!
//! **Stale grants are surfaced, never swallowed.** A grant naming a source
//! that no longer exists is inert at the dispatch gate (the tool refuses the
//! unknown name), but it is *not* nothing: it is a leftover the operator
//! should see. `grants.list` reports those under `stale` rather than quietly
//! filtering them out of the per-source lists.
//!
//! **One write path.** Every mutation goes through
//! [`crate::channel_reply::update_agent_toml_with`] — the same read-parse-
//! mutate-atomic-rename-rescan path `agents.update` uses — so a grant change
//! is hot-reloaded into the in-memory registry exactly like any other
//! `agent.toml` edit. (The gate itself needs no reload at all: it re-reads
//! `agent.toml` per MCP call. See the module docs of `mcp_dispatch`'s
//! `load_agent_gate_config`.)
//!
//! Only the `[capabilities] db_sources` key is touched; every other table in
//! the file is carried through the round-trip untouched. A grant list that
//! becomes empty has its key removed rather than left as `[]`, matching the
//! `skip_serializing_if = "Vec::is_empty"` shape `CapabilitiesConfig` writes.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_db::LoadedDbSources;
use duduclaw_db::config::is_valid_source_name;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::protocol::WsFrame;

/// The registry handle the agent.toml write path needs.
pub type Registry = Arc<RwLock<AgentRegistry>>;

/// The `[capabilities]` key these grants live in.
const GRANT_KEY: &str = "db_sources";

/// Activity-feed event type emitted for one grant change.
pub const ACTIVITY_EVENT: &str = "db_source_grant_changed";

/// The two methods this module owns, kept next to `db_sources_rpc::METHODS`
/// so the dispatch arm and the modules can never disagree about the surface.
pub const METHODS: &[&str] = &["db_sources.grants.list", "db_sources.grants.set"];

// ── Agent enumeration ───────────────────────────────────────────────────────

/// One agent's identity plus the data-source grants it currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGrantRow {
    pub id: String,
    pub display_name: String,
    /// Raw `[agent] role` (empty when the file omits it).
    pub role: String,
    /// Raw `[agent] status`, lowercased. `"active"` when the file omits it —
    /// a scannable `agent.toml` always carries one, so the fallback only ever
    /// applies to a malformed file, where claiming "archived" would be worse.
    pub status: String,
    pub archived: bool,
    /// `[capabilities] db_sources`, trimmed and de-duplicated.
    pub granted: Vec<String>,
}

/// Enumerate `<home>/agents/*` and read each agent's grants.
///
/// Deliberately filesystem-driven rather than registry-driven: an agent whose
/// `agent.toml` fails the strict `AgentConfig` parse is absent from the
/// registry, and an agent holding a database grant must not become invisible
/// on this screen because of an unrelated malformed field. Reading goes
/// through the typed [`duduclaw_core::agent_toml`] reader (no new hand-rolled
/// `toml::Value` shadow reader — CLAUDE.md "agent.toml 影子直讀統一"), which is
/// total: a broken file yields defaults instead of an error.
///
/// `_`-prefixed directories (`_trash`, `_defaults`, …) are skipped, the same
/// rule `AgentRegistry::scan` and the startup hook installer apply.
///
/// Sorted by agent id.
pub fn scan_agent_grants(home_dir: &Path) -> Vec<AgentGrantRow> {
    let agents_dir = home_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };
    let mut rows: Vec<AgentGrantRow> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if id.starts_with('_') || !duduclaw_core::is_valid_agent_id(id) {
            continue;
        }
        if !path.join("agent.toml").is_file() {
            continue;
        }
        let sections = duduclaw_core::agent_toml::load(&path);
        let agent = sections.agent.unwrap_or_default();
        let display_name = agent
            .display_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.to_string());
        let role = agent.role.map(|s| s.trim().to_string()).unwrap_or_default();
        let status = agent
            .status
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "active".to_string());
        // Mirrors `agents.list`'s `matches!(status, AgentStatus::Archived)` —
        // `AgentStatus` is `rename_all = "snake_case"`, so the on-disk token
        // for that variant is exactly `"archived"`.
        let archived = status == "archived";
        rows.push(AgentGrantRow {
            id: id.to_string(),
            display_name,
            role,
            status,
            archived,
            granted: dedup_trimmed(sections.capabilities.db_sources),
        });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// Trim, drop empties, de-duplicate — preserving first-seen order.
fn dedup_trimmed(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for item in raw {
        let s = item.trim();
        if s.is_empty() {
            continue;
        }
        if !out.iter().any(|e| e == s) {
            out.push(s.to_string());
        }
    }
    out
}

// ── grants.list ─────────────────────────────────────────────────────────────

/// `db_sources.grants.list` — the whole grant matrix in one read.
pub async fn list(home_dir: &Path) -> WsFrame {
    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    let rows = scan_agent_grants(home_dir);
    let configured: BTreeSet<&str> = loaded.sources.iter().map(|s| s.name.as_str()).collect();

    // Every configured source, including ones nobody holds (`agents: []`) —
    // an unused source is exactly what this screen exists to let an operator
    // fix. `rows` is already id-sorted, so the per-source lists are too.
    let sources: Vec<Value> = loaded
        .sources
        .iter()
        .map(|s| {
            let agents: Vec<&str> = rows
                .iter()
                .filter(|r| holds(r, &s.name))
                .map(|r| r.id.as_str())
                .collect();
            json!({ "name": s.name, "label": s.label, "agents": agents })
        })
        .collect();

    let agents: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "display_name": r.display_name,
                "role": r.role,
                "status": r.status,
                "archived": r.archived,
            })
        })
        .collect();

    // Grants naming a source that is not configured. Keyed by agent id.
    let mut stale = serde_json::Map::new();
    for r in &rows {
        let extra: Vec<&str> = r
            .granted
            .iter()
            .filter(|g| !configured.contains(g.as_str()))
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            stale.insert(r.id.clone(), json!(extra));
        }
    }

    // Additive: the same per-source load errors `db_sources.list` already
    // returns. Without them a `config.toml` that failed to parse would render
    // as "zero sources, every grant stale" with no explanation.
    let errors: Vec<Value> = loaded
        .errors
        .iter()
        .map(|e| json!({ "name": e.name, "message": e.message }))
        .collect();

    WsFrame::ok_response(
        "",
        json!({
            "sources": sources,
            "agents": agents,
            "stale": Value::Object(stale),
            "errors": errors,
        }),
    )
}

/// Exact-equality membership test (coding convention 2 — never a substring).
fn holds(row: &AgentGrantRow, source: &str) -> bool {
    row.granted.iter().any(|g| g == source)
}

// ── grants.set ──────────────────────────────────────────────────────────────

/// `db_sources.grants.set` — replace the grant set of ONE source.
///
/// Agents in `agents` that lack the grant gain it; agents that hold it and are
/// not listed lose it; every other source in every agent's list is untouched.
pub async fn set(registry: &Registry, home_dir: &Path, params: Value) -> WsFrame {
    let name = match required_source_name(&params) {
        Ok(n) => n,
        Err(msg) => return WsFrame::error_response("", &msg),
    };
    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    let Some(entry) = loaded.get(&name) else {
        return WsFrame::error_response("", &unknown_source_message(&name, &loaded));
    };
    // Write the id exactly as `config.toml` spells it, never the caller's
    // casing, so what lands in agent.toml is what the MCP gate looks up.
    let canonical = entry.name.clone();

    let requested = match parse_agent_list(home_dir, &params) {
        Ok(v) => v,
        Err(msg) => return WsFrame::error_response("", &msg),
    };

    let rows = scan_agent_grants(home_dir);
    let holders: Vec<String> = rows
        .iter()
        .filter(|r| holds(r, &canonical))
        .map(|r| r.id.clone())
        .collect();

    let mut added: Vec<String> = requested
        .iter()
        .filter(|id| !holders.iter().any(|h| h == *id))
        .cloned()
        .collect();
    let mut removed: Vec<String> = holders
        .iter()
        .filter(|id| !requested.iter().any(|r| r == *id))
        .cloned()
        .collect();
    let mut unchanged: Vec<String> = requested
        .iter()
        .filter(|id| holders.iter().any(|h| h == *id))
        .cloned()
        .collect();
    added.sort();
    removed.sort();
    unchanged.sort();

    // Fail closed BEFORE the first write: every agent we are about to touch
    // must be loadable through the registry, which is what the write path
    // resolves the agent directory from. A removal target that is missing
    // there is a genuinely broken `agent.toml`, and aborting is better than
    // applying half the change set.
    {
        let reg = registry.read().await;
        for id in added.iter().chain(removed.iter()) {
            if reg.get(id).is_none() {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "AI 員工「{id}」的 agent.toml 無法載入（格式有誤或已被移除），\
                         授權未變更。請先修正該檔案再重試。"
                    ),
                );
            }
        }
    }

    for id in &added {
        if let Err(e) = write_grant(registry, id, &canonical, true).await {
            return WsFrame::error_response("", &format!("寫入「{id}」的資料來源授權失敗：{e}"));
        }
    }
    for id in &removed {
        if let Err(e) = write_grant(registry, id, &canonical, false).await {
            return WsFrame::error_response("", &format!("移除「{id}」的資料來源授權失敗：{e}"));
        }
    }

    if !added.is_empty() || !removed.is_empty() {
        post_activity(home_dir, &canonical, &added, &removed).await;
    }

    tracing::info!(
        source = %canonical,
        added = added.len(),
        removed = removed.len(),
        "db_sources.grants.set completed"
    );
    WsFrame::ok_response(
        "",
        json!({
            "ok": true,
            "name": canonical,
            "added": added,
            "removed": removed,
            "unchanged": unchanged,
        }),
    )
}

/// Revoke `source` from every agent that holds it. Used by
/// `db_sources.remove`, where leaving the grants behind would strand a name
/// no configuration explains any more.
///
/// Returns `(revoked, failed)`. Failures are reported, never swallowed — but
/// they do not abort: the source is already gone from `config.toml` by the
/// time this runs, and a leftover grant is inert (the tool refuses an
/// unconfigured name) rather than a privilege.
pub async fn revoke_everywhere(
    registry: &Registry,
    home_dir: &Path,
    source: &str,
) -> (Vec<String>, Vec<String>) {
    let mut revoked = Vec::new();
    let mut failed = Vec::new();
    for row in scan_agent_grants(home_dir) {
        if !holds(&row, source) {
            continue;
        }
        match write_grant(registry, &row.id, source, false).await {
            Ok(()) => revoked.push(row.id),
            Err(e) => {
                tracing::warn!(agent = %row.id, source, error = %e, "failed to revoke db source grant");
                failed.push(row.id);
            }
        }
    }
    if !revoked.is_empty() {
        post_activity(home_dir, source, &[], &revoked).await;
    }
    (revoked, failed)
}

// ── Write path ──────────────────────────────────────────────────────────────

/// Add or remove ONE source from ONE agent's `[capabilities] db_sources`.
///
/// Goes through `update_agent_toml_with`: agent-id validation, read, parse,
/// mutate, atomic temp-file rename, registry rescan. Nothing else in the file
/// is read or written by the closure.
async fn write_grant(
    registry: &Registry,
    agent_id: &str,
    source: &str,
    grant: bool,
) -> Result<(), String> {
    let source = source.to_string();
    crate::channel_reply::update_agent_toml_with(registry, agent_id, move |table| {
        let section = table
            .entry("capabilities")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "agent.toml 的 [capabilities] 不是一個表格".to_string())?;
        let mut list = read_grant_array(section);
        if grant {
            if !list.iter().any(|g| g == &source) {
                list.push(source.clone());
            }
        } else {
            list.retain(|g| g != &source);
        }
        if list.is_empty() {
            section.remove(GRANT_KEY);
        } else {
            section.insert(
                GRANT_KEY.into(),
                toml::Value::Array(list.into_iter().map(toml::Value::String).collect()),
            );
        }
        Ok(())
    })
    .await
    .map(|_| ())
}

/// Read `[capabilities] db_sources` out of a raw table, tolerating the same
/// stray non-string element `CapabilitiesConfig`'s lenient deserializer drops.
fn read_grant_array(section: &toml::Table) -> Vec<String> {
    let raw = section
        .get(GRANT_KEY)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    dedup_trimmed(raw)
}

// ── Shared validation (also used by `agents.update`) ─────────────────────────

/// Normalize a `capabilities.db_sources` JSON array: every entry a non-empty
/// string, trimmed, de-duplicated preserving first-seen order.
///
/// Pure — no config lookup. `apply_capabilities_to_table` (sync, no
/// `home_dir`) and [`validate_capability_grants`] share it so the value that
/// is validated and the value that is written can never drift.
pub fn normalize_grant_list(raw: &Value) -> Result<Vec<String>, String> {
    let arr = raw
        .as_array()
        .ok_or_else(|| "capabilities.db_sources must be an array".to_string())?;
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| "capabilities.db_sources entries must be strings".to_string())?
            .trim();
        if s.is_empty() {
            return Err("capabilities.db_sources entries must be non-empty".to_string());
        }
        if !out.iter().any(|e| e == s) {
            out.push(s.to_string());
        }
    }
    Ok(out)
}

/// [`normalize_grant_list`] plus the check its siblings (`allowed_tools`, …)
/// deliberately do not have: every entry must name a configured
/// `config.toml [db_sources.<id>]` block.
///
/// Fail-closed — one unknown id rejects the whole update, so a typo cannot
/// land a grant that silently does nothing. The message lists the configured
/// ids and ONLY the ids.
pub async fn validate_capability_grants(
    home_dir: &Path,
    raw: &Value,
) -> Result<Vec<String>, String> {
    let list = normalize_grant_list(raw)?;
    if list.is_empty() {
        // Revoking every grant needs no source registry at all — and must keep
        // working when `config.toml` is unreadable.
        return Ok(list);
    }
    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    let unknown: Vec<&str> = list
        .iter()
        .filter(|n| !loaded.sources.iter().any(|s| &s.name == *n))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return Ok(list);
    }
    Err(unknown_grant_message(&unknown, &loaded))
}

/// Rejection text for unknown grant ids. Ids only — never a connection string.
fn unknown_grant_message(unknown: &[&str], loaded: &LoadedDbSources) -> String {
    let bad = unknown.join("、");
    let configured = loaded.names();
    if configured.is_empty() {
        return format!(
            "資料來源授權「{bad}」不存在：目前沒有任何已設定的資料來源。\
             請先在儀表板「設定 → 去識別化 → 資料來源」建立。"
        );
    }
    format!(
        "資料來源授權「{bad}」不存在。目前已設定的資料來源：{}。",
        configured.join("、")
    )
}

// ── Parameter parsing ───────────────────────────────────────────────────────

fn required_source_name(params: &Value) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return Err("缺少 name 參數（資料來源名稱）".to_string());
    }
    if !is_valid_source_name(name) {
        return Err(format!(
            "資料來源名稱「{name}」不合法（只允許小寫英文字母開頭，之後是小寫字母、數字或底線）"
        ));
    }
    Ok(name.to_string())
}

fn unknown_source_message(name: &str, loaded: &LoadedDbSources) -> String {
    let configured = loaded.names();
    if configured.is_empty() {
        return format!("資料來源「{name}」不存在：目前沒有任何已設定的資料來源。");
    }
    format!(
        "資料來源「{name}」不存在。目前已設定的資料來源：{}。",
        configured.join("、")
    )
}

/// Validate the `agents` array: every entry a valid agent id with an
/// `agent.toml` on disk. De-duplicated, first-seen order preserved.
fn parse_agent_list(home_dir: &Path, params: &Value) -> Result<Vec<String>, String> {
    let arr = params
        .get("agents")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "缺少 agents 參數（陣列；空陣列代表全部取消授權）".to_string())?;
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for item in arr {
        let id = item
            .as_str()
            .ok_or_else(|| "agents 陣列只能包含字串（AI 員工 ID）".to_string())?
            .trim();
        if id.is_empty() {
            return Err("agents 陣列不可包含空字串".to_string());
        }
        if !duduclaw_core::is_valid_agent_id(id) {
            return Err(format!("AI 員工 ID「{id}」不合法"));
        }
        if !home_dir
            .join("agents")
            .join(id)
            .join("agent.toml")
            .is_file()
        {
            return Err(format!("AI 員工「{id}」不存在（找不到 agent.toml）"));
        }
        if !out.iter().any(|e| e == id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

// ── Activity feed ───────────────────────────────────────────────────────────

/// Append one Activity Feed row for a grant change. Best-effort, exactly like
/// `redaction_integration::post_redaction_activity` — an unavailable task
/// store must not fail an authorization change that already landed on disk.
async fn post_activity(home_dir: &Path, source: &str, added: &[String], removed: &[String]) {
    let store = match crate::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "db source grant activity: task store unavailable (non-fatal)");
            return;
        }
    };
    let mut parts: Vec<String> = Vec::new();
    if !added.is_empty() {
        parts.push(format!("新增授權 {}", added.join("、")));
    }
    if !removed.is_empty() {
        parts.push(format!("取消授權 {}", removed.join("、")));
    }
    let row = crate::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: ACTIVITY_EVENT.to_string(),
        agent_id: String::new(),
        task_id: None,
        summary: format!("資料來源「{source}」：{}", parts.join("；")),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: serde_json::to_string(
            &json!({ "name": source, "added": added, "removed": removed }),
        )
        .ok(),
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!(error = %e, "db source grant activity: append failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_ok(frame: &WsFrame) -> bool {
        matches!(frame, WsFrame::Response { ok: true, .. })
    }

    fn frame_error(frame: &WsFrame) -> String {
        match frame {
            WsFrame::Response { error: Some(e), .. } => e
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string()),
            _ => String::new(),
        }
    }

    fn result(frame: &WsFrame) -> Value {
        match frame {
            WsFrame::Response {
                payload: Some(p), ..
            } => p.clone(),
            _ => Value::Null,
        }
    }

    /// A complete, registry-scannable agent.toml (`AgentRegistry` loads it
    /// through the strict `AgentConfig` deserializer, not a raw table).
    fn seed_agent(home: &Path, id: &str, grants: &[&str]) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let caps = if grants.is_empty() {
            "[capabilities]\ncomputer_use = false\n".to_string()
        } else {
            let list = grants
                .iter()
                .map(|g| format!("\"{g}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[capabilities]\ncomputer_use = false\ndb_sources = [{list}]\n")
        };
        let toml = format!(
            r#"[agent]
name = "{id}"
display_name = "{id}-name"
role = "worker"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true

{caps}"#
        );
        std::fs::write(dir.join("agent.toml"), toml).unwrap();
    }

    fn seed_config(home: &Path, sources: &[(&str, &str)]) {
        let mut out = String::from("[some_unrelated]\nkeep = true\n\n");
        for (name, label) in sources {
            out.push_str(&format!(
                "[db_sources.{name}]\nlabel = \"{label}\"\ndriver = \"sqlite\"\nurl = \"/tmp/{name}.db\"\nallowed_tables = [\"t\"]\n\n"
            ));
        }
        std::fs::write(home.join("config.toml"), out).unwrap();
    }

    async fn registry_for(home: &Path) -> Registry {
        // `scan` errors on a missing directory; a home with zero agents is a
        // legitimate fixture (see `grants_set_requires_the_agents_parameter`).
        std::fs::create_dir_all(home.join("agents")).unwrap();
        let mut reg = AgentRegistry::new(home.join("agents"));
        reg.scan().await.unwrap();
        Arc::new(RwLock::new(reg))
    }

    fn grants_on_disk(home: &Path, id: &str) -> Vec<String> {
        let raw = std::fs::read_to_string(home.join("agents").join(id).join("agent.toml")).unwrap();
        let table: toml::Table = raw.parse().unwrap();
        table
            .get("capabilities")
            .and_then(|c| c.as_table())
            .map(read_grant_array)
            .unwrap_or_default()
    }

    // ── grants.list ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn grants_list_aggregates_sources_agents_and_stale() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "客戶 CRM"), ("lonely", "沒人用")]);
        seed_agent(home.path(), "main", &["crm_pg"]);
        seed_agent(home.path(), "sales", &["crm_pg"]);
        seed_agent(home.path(), "ops", &["gone_source"]);
        // `_trash` must never be enumerated as an agent.
        seed_agent(home.path(), "keeper", &[]);
        std::fs::rename(
            home.path().join("agents").join("keeper"),
            home.path().join("agents").join("_trash"),
        )
        .unwrap();

        let frame = list(home.path()).await;
        assert!(frame_ok(&frame), "{frame:?}");
        let data = result(&frame);

        let sources = data["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "{sources:?}");
        let crm = sources.iter().find(|s| s["name"] == "crm_pg").unwrap();
        assert_eq!(crm["label"], "客戶 CRM");
        assert_eq!(crm["agents"], json!(["main", "sales"]));
        let lonely = sources.iter().find(|s| s["name"] == "lonely").unwrap();
        assert_eq!(lonely["agents"], json!([]), "unheld source still listed");

        let agents = data["agents"].as_array().unwrap();
        let ids: Vec<&str> = agents.iter().map(|a| a["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec!["main", "ops", "sales"],
            "_trash skipped, id-sorted"
        );
        let ops = agents.iter().find(|a| a["id"] == "ops").unwrap();
        assert_eq!(ops["display_name"], "ops-name");
        assert_eq!(ops["role"], "worker");
        assert_eq!(ops["status"], "active");
        assert_eq!(ops["archived"], json!(false));

        assert_eq!(data["stale"], json!({ "ops": ["gone_source"] }));
    }

    #[tokio::test]
    async fn grants_list_marks_archived_agents() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "old", &[]);
        let p = home.path().join("agents").join("old").join("agent.toml");
        let raw = std::fs::read_to_string(&p).unwrap();
        std::fs::write(
            &p,
            raw.replace("status = \"active\"", "status = \"archived\""),
        )
        .unwrap();

        let data = result(&list(home.path()).await);
        let old = data["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "old")
            .unwrap();
        assert_eq!(old["status"], "archived");
        assert_eq!(old["archived"], json!(true));
    }

    // ── grants.set ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn grants_set_adds_removes_and_reports_unchanged() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM"), ("other", "Other")]);
        seed_agent(home.path(), "main", &[]);
        seed_agent(home.path(), "sales", &["crm_pg", "other"]);
        seed_agent(home.path(), "ops", &["crm_pg"]);
        let reg = registry_for(home.path()).await;

        let frame = set(
            &reg,
            home.path(),
            json!({ "name": "crm_pg", "agents": ["sales", "main"] }),
        )
        .await;
        assert!(frame_ok(&frame), "{}", frame_error(&frame));
        let data = result(&frame);
        assert_eq!(data["ok"], json!(true));
        assert_eq!(data["name"], "crm_pg");
        assert_eq!(data["added"], json!(["main"]));
        assert_eq!(data["removed"], json!(["ops"]));
        assert_eq!(data["unchanged"], json!(["sales"]));

        assert_eq!(grants_on_disk(home.path(), "main"), vec!["crm_pg"]);
        // The agent's OTHER grant must survive untouched.
        assert_eq!(
            grants_on_disk(home.path(), "sales"),
            vec!["crm_pg".to_string(), "other".to_string()]
        );
        // Revoked down to nothing ⇒ the key is removed, not left as `[]`.
        assert!(grants_on_disk(home.path(), "ops").is_empty());
        let ops_raw =
            std::fs::read_to_string(home.path().join("agents").join("ops").join("agent.toml"))
                .unwrap();
        assert!(!ops_raw.contains("db_sources"), "{ops_raw}");
    }

    #[tokio::test]
    async fn grants_set_leaves_unrelated_agent_toml_content_intact() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "main", &[]);
        let path = home.path().join("agents").join("main").join("agent.toml");
        let before: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let reg = registry_for(home.path()).await;

        let frame = set(
            &reg,
            home.path(),
            json!({ "name": "crm_pg", "agents": ["main"] }),
        )
        .await;
        assert!(frame_ok(&frame), "{}", frame_error(&frame));

        let after: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // Every section except `[capabilities]` survives with identical parsed
        // content. (Not byte-for-byte: the shared write path re-serializes the
        // whole table with `toml::to_string_pretty`, so formatting and
        // comments are normalized for every `agents.update` too.)
        for key in [
            "agent",
            "model",
            "container",
            "heartbeat",
            "budget",
            "permissions",
            "evolution",
        ] {
            assert_eq!(before[key], after[key], "section [{key}] changed");
        }
        // And inside `[capabilities]`, only the grant key was added.
        let cap_before = before["capabilities"].as_table().unwrap();
        let cap_after = after["capabilities"].as_table().unwrap();
        assert_eq!(cap_before["computer_use"], cap_after["computer_use"]);
        assert_eq!(
            cap_after[GRANT_KEY],
            toml::Value::try_from(["crm_pg"]).unwrap()
        );
    }

    #[tokio::test]
    async fn grants_set_rejects_unknown_source_without_writing() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "main", &[]);
        let reg = registry_for(home.path()).await;

        let frame = set(
            &reg,
            home.path(),
            json!({ "name": "nope", "agents": ["main"] }),
        )
        .await;
        assert!(!frame_ok(&frame));
        let err = frame_error(&frame);
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("crm_pg"), "must list configured ids: {err}");
        assert!(grants_on_disk(home.path(), "main").is_empty());
    }

    #[tokio::test]
    async fn grants_set_rejects_unknown_agent_without_writing() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "main", &[]);
        let reg = registry_for(home.path()).await;

        for bad in ["ghost", "../etc", ""] {
            let frame = set(
                &reg,
                home.path(),
                json!({ "name": "crm_pg", "agents": ["main", bad] }),
            )
            .await;
            assert!(!frame_ok(&frame), "must reject agent id {bad:?}");
            assert!(
                grants_on_disk(home.path(), "main").is_empty(),
                "nothing may be written when one id is bad ({bad:?})"
            );
        }
    }

    #[tokio::test]
    async fn grants_set_empty_list_revokes_everyone() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "main", &["crm_pg"]);
        seed_agent(home.path(), "sales", &["crm_pg"]);
        let reg = registry_for(home.path()).await;

        let data = result(&set(&reg, home.path(), json!({ "name": "crm_pg", "agents": [] })).await);
        assert_eq!(data["removed"], json!(["main", "sales"]));
        assert!(grants_on_disk(home.path(), "main").is_empty());
        assert!(grants_on_disk(home.path(), "sales").is_empty());
    }

    /// The change is auditable: one Activity Feed row naming the source and
    /// both sides of the diff. A no-op `set` posts nothing.
    #[tokio::test]
    async fn grants_set_posts_one_activity_row() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        seed_agent(home.path(), "main", &[]);
        let reg = registry_for(home.path()).await;

        assert!(frame_ok(
            &set(
                &reg,
                home.path(),
                json!({ "name": "crm_pg", "agents": ["main"] })
            )
            .await
        ));
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let (rows, total) = store
            .list_activity(None, Some(ACTIVITY_EVENT), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1, "{rows:?}");
        assert!(rows[0].summary.contains("crm_pg"), "{:?}", rows[0]);
        let meta: Value = serde_json::from_str(rows[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["name"], "crm_pg");
        assert_eq!(meta["added"], json!(["main"]));
        assert_eq!(meta["removed"], json!([]));

        // Re-applying the same grant set changes nothing → no second row.
        assert!(frame_ok(
            &set(
                &reg,
                home.path(),
                json!({ "name": "crm_pg", "agents": ["main"] })
            )
            .await
        ));
        let (_, total) = store
            .list_activity(None, Some(ACTIVITY_EVENT), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn grants_set_requires_the_agents_parameter() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        let reg = registry_for(home.path()).await;
        let frame = set(&reg, home.path(), json!({ "name": "crm_pg" })).await;
        assert!(!frame_ok(&frame));
    }

    // ── revoke_everywhere ───────────────────────────────────────────────

    #[tokio::test]
    async fn revoke_everywhere_clears_only_the_named_source() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM"), ("other", "Other")]);
        seed_agent(home.path(), "main", &["crm_pg", "other"]);
        seed_agent(home.path(), "sales", &["other"]);
        let reg = registry_for(home.path()).await;

        let (revoked, failed) = revoke_everywhere(&reg, home.path(), "crm_pg").await;
        assert_eq!(revoked, vec!["main".to_string()]);
        assert!(failed.is_empty());
        assert_eq!(grants_on_disk(home.path(), "main"), vec!["other"]);
        assert_eq!(grants_on_disk(home.path(), "sales"), vec!["other"]);
    }

    // ── validation helpers ──────────────────────────────────────────────

    #[test]
    fn normalize_trims_dedups_and_preserves_order() {
        let got = normalize_grant_list(&json!([" b ", "a", "b"])).unwrap();
        assert_eq!(got, vec!["b".to_string(), "a".to_string()]);
        assert!(normalize_grant_list(&json!("b")).is_err());
        assert!(normalize_grant_list(&json!([1])).is_err());
        assert!(normalize_grant_list(&json!(["  "])).is_err());
    }

    #[tokio::test]
    async fn validate_capability_grants_rejects_unknown_and_accepts_known() {
        let home = tempfile::tempdir().unwrap();
        seed_config(home.path(), &[("crm_pg", "CRM")]);
        assert_eq!(
            validate_capability_grants(home.path(), &json!(["crm_pg"]))
                .await
                .unwrap(),
            vec!["crm_pg".to_string()]
        );
        let err = validate_capability_grants(home.path(), &json!(["crm_pg", "ghost"]))
            .await
            .unwrap_err();
        assert!(err.contains("ghost"), "{err}");
        assert!(err.contains("crm_pg"), "{err}");
        // Empty list (revoke-all) never needs the source registry.
        assert!(
            validate_capability_grants(home.path(), &json!([]))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
