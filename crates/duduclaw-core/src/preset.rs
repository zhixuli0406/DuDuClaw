//! Agent preset ("職務組合") store and resolution — WP-6F P1.
//!
//! # Why this exists
//!
//! `commercial/docs/DESIGN-agent-presets-2026-08.md` names the problem: a
//! deployed agent's `agent.toml` is a one-time copy of a shared department
//! kit — the moment `cp -r` finishes, source and copy are permanently
//! disconnected. This module turns "copy once" into "reference continuously":
//! a named, versioned configuration bundle (a *preset*) that an agent binds
//! to, resolved fresh on every load.
//!
//! # Three layers (design §1)
//!
//! ```text
//! built-in Default::default()   ← least specific
//!   ⊕ preset (if a binding exists)
//!   ⊕ per-agent agent.toml explicit fields   ← most specific, always wins
//! ```
//!
//! # Authority vs. mirror (design §3.1, copied from `org_store`'s shape)
//!
//! - **Authority**: `<home>/preset_bindings.toml`, keyed by agent **directory
//!   name** (matching `org_store`'s convention). This is what
//!   [`resolve_for_agent`] reads.
//! - **Mirror**: `<agent_dir>/agent.toml [preset]` — informational only,
//!   written by callers for human readability. Never read by this module.
//! - **Resolved artifact**: `<home>/agent_resolved/<agent_id>.toml` — the
//!   merged (preset ⊕ per-agent) table, materialized so the twelve
//!   `agent_toml::AgentTomlSections` shadow readers (R2) see resolved values
//!   too. Deliberately **outside** the agent's own directory — see
//!   [`agent_resolved_path`] for why.
//!
//! # Security boundary (design §2.2 / R4)
//!
//! `[agent] name` / `reports_to` / `department` — the org-authority fields
//! frozen by [`crate::org_field_guard`] — must never flow through a preset;
//! that would be a batch-privilege-escalation channel (one preset edit
//! silently re-parents every bound agent). [`sanitize_preset_table`] strips
//! the entire `[agent]` table from any preset content; if it carried any of
//! [`crate::org_field_guard::AGENT_ORG_FIELDS`], resolution refuses the
//! **whole** preset (fail-closed, not just those keys) and the caller must
//! record an audit event.
//!
//! # What is deliberately NOT here (P1 scope)
//!
//! No switching UI, no CAS/rollback, no version-flow tooling, no contract
//! overlay / prompt-section / eval-suite-ref application (those are P2/P3
//! per the design's route table). This module only does "reference +
//! layer + leave a trace"; the trace-writing itself (audit event, Activity
//! Feed row, agent-visible dynamic-tail line) lives in the calling crates
//! that already own those subsystems (`duduclaw-security::audit`,
//! `duduclaw-gateway::task_store`, `duduclaw-gateway::preset_prompt`) —
//! `duduclaw-core` cannot depend on either.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::fs_lock::with_file_lock;
use crate::org_field_guard::AGENT_ORG_FIELDS;

/// Directory under `<home>` holding preset content: `<home>/presets/<id>/preset.toml`.
pub const PRESETS_DIR: &str = "presets";
/// Basename of a preset's content file.
pub const PRESET_FILE: &str = "preset.toml";
/// Basename of the authoritative binding store under `<home>`.
pub const PRESET_BINDINGS_FILE: &str = "preset_bindings.toml";
/// Basename of the append-only binding history under `<home>`.
pub const PRESET_BINDINGS_HISTORY_FILE: &str = "preset_bindings_history.jsonl";
/// Directory under `<home>` holding materialized resolved configs, one file
/// per agent. See [`agent_resolved_path`] for why this is not inside the
/// agent's own directory.
pub const AGENT_RESOLVED_DIR: &str = "agent_resolved";

/// Schema version written into `preset_bindings.toml`. A file carrying a
/// *higher* version was written by a newer DuDuClaw; this build refuses to
/// interpret it (fail-safe: resolution falls back to `Unbound` for every
/// agent) rather than guess — same posture as `org_store`.
pub const PRESET_BINDINGS_SCHEMA: i64 = 1;

// ── Per-agent-only fields (design §2.2) — never allowed to flow through a
// preset. Kept as a table rather than scattered `if` checks so the sanitizer
// and its tests stay a single source of truth for the boundary.
const ORG_TABLE: &str = "agent";
const SECRET_TABLES: &[&str] = &["channels", "odoo"];
const PER_AGENT_SCALAR_FIELDS: &[(&str, &str)] = &[
    ("os_watch", "paths"),
    ("model", "account_pool"),
    ("container", "additional_mounts"),
    ("proactive", "notify_channel"),
    ("proactive", "notify_chat_id"),
    ("proactive", "notify_thread_id"),
    ("permissions", "can_modify_own_soul"),
];

/// `<home>/presets`.
pub fn presets_dir(home: &Path) -> PathBuf {
    home.join(PRESETS_DIR)
}

/// `<home>/presets/<id>`.
pub fn preset_dir(home: &Path, id: &str) -> PathBuf {
    presets_dir(home).join(id)
}

/// `<home>/presets/<id>/preset.toml`.
pub fn preset_file_path(home: &Path, id: &str) -> PathBuf {
    preset_dir(home, id).join(PRESET_FILE)
}

/// `<home>/preset_bindings.toml`.
pub fn bindings_path(home: &Path) -> PathBuf {
    home.join(PRESET_BINDINGS_FILE)
}

/// `<home>/preset_bindings_history.jsonl`.
pub fn bindings_history_path(home: &Path) -> PathBuf {
    home.join(PRESET_BINDINGS_HISTORY_FILE)
}

/// `<home>/agent_resolved/<agent_id>.toml`.
///
/// **Deliberately outside `<home>/agents/<agent_id>/`.** The twelve
/// `agent_toml::AgentTomlSections` shadow readers (capability grants,
/// guardrails, MCP external servers, GVU noise-band, …) are redirected to
/// prefer this file when it exists (see `agent_toml::load`). If it lived
/// inside the agent's own directory, an agent with `Bash`/`Write` access to
/// its own workspace could edit it directly and grant itself capabilities no
/// preset actually authorizes — the exact laundering channel `org_store`
/// already had to close for `reports_to`/`department` by moving `org.toml`
/// outside every agent's cwd. A `workspace-write` sandbox scopes writes to
/// the agent's own directory (and the project it's working in), so a
/// sibling-of-`agents/` path is out of reach by construction, for every
/// runtime (Claude/codex/gemini/antigravity/openai-compat alike — unlike a
/// `PreToolUse` hook, which only exists for the Claude runtime).
pub fn agent_resolved_path(home: &Path, agent_id: &str) -> PathBuf {
    home.join(AGENT_RESOLVED_DIR).join(format!("{agent_id}.toml"))
}

/// Derive `<home>` from an agent directory, but only when the directory is
/// literally `<home>/agents/<id>` — the standard registry layout. Anything
/// else (ephemeral scaffolds under `agents/.ephemeral/<id>`, ad-hoc test
/// fixtures) yields `None` rather than a guessed-wrong path: preset
/// resolution simply does not apply to such agents (byte-identical to
/// "unbound"), which is a safe degradation, not a broken one.
pub fn agent_home_dir(agent_dir: &Path) -> Option<PathBuf> {
    let parent = agent_dir.parent()?;
    if parent.file_name().and_then(|n| n.to_str()) != Some("agents") {
        return None;
    }
    parent.parent().map(|p| p.to_path_buf())
}

// ── Free built-in preset (no license gate) — P2 ─────────────────────────
//
// The 9 department kits under `commercial/templates-premium/presets/` are
// gated by the `premium_templates` license feature AND only reachable when
// the gitignored `commercial/` nested repo is checked out (see
// `duduclaw-cli::premium_templates::find_premium_templates_dir`). Neither
// applies here: this one preset ships inside the public repo's `templates/`
// tree (L1 public) and is compiled directly into the binary via
// `include_str!`, the same mechanism already used for
// `templates/wiki/CLAUDE_WIKI.md` in `duduclaw-cli`'s agent scaffold. Every
// OSS build has it — no license, no `commercial/` checkout, no network call.

/// Id of the one free built-in preset this crate ships: a system-operator
/// persona (`agent.toml [capabilities] system_operator = true`, `os_operator`
/// module docs' own recommended snippet, now installable with one command
/// instead of hand-editing `agent.toml`).
pub const SYSTEM_OPERATOR_PRESET_ID: &str = "system-operator";

const SYSTEM_OPERATOR_PRESET_TOML: &str =
    include_str!("../../../templates/presets/system-operator/preset.toml");

/// Suggested SOUL.md persona body for an agent created with the
/// `system-operator` preset. Presets only cover `agent.toml`-shaped config
/// (module docs above) — SOUL.md is a separate per-agent file, never merged
/// by [`resolve_for_agent`] — so this is exposed alongside the preset rather
/// than folded into [`ParsedPreset`]. See [`builtin_soul_template`].
const SYSTEM_OPERATOR_SOUL_TEMPLATE: &str =
    include_str!("../../../templates/presets/system-operator/SOUL.md");

/// Write the one free built-in preset to
/// `<home>/presets/system-operator/preset.toml`, unless a file is already
/// there (or unconditionally when `force`, mirroring `duduclaw preset
/// install-builtin`'s existing `--force` semantics for the premium kits).
/// Returns `true` when a file was actually written. No license/feature
/// gate — this preset is free by design, unlike the premium department kits.
pub fn install_system_operator_preset(home: &Path, force: bool) -> io::Result<bool> {
    let dest = preset_file_path(home, SYSTEM_OPERATOR_PRESET_ID);
    if dest.is_file() && !force {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, SYSTEM_OPERATOR_PRESET_TOML)?;
    Ok(true)
}

/// Suggested SOUL.md body for a known free built-in preset id, or `None`
/// for anything else (including every premium preset id — those carry no
/// persona template here and callers fall back to their own default SOUL.md
/// template).
pub fn builtin_soul_template(preset_id: &str) -> Option<&'static str> {
    if preset_id == SYSTEM_OPERATOR_PRESET_ID {
        Some(SYSTEM_OPERATOR_SOUL_TEMPLATE)
    } else {
        None
    }
}

// ── Sanitization ─────────────────────────────────────────────────────────

/// Result of stripping per-agent-only content out of a preset's raw table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SanitizeReport {
    /// The table with every per-agent-only field removed.
    pub table: toml::value::Table,
    /// `AGENT_ORG_FIELDS` present with a non-empty value under `[agent]`.
    /// Non-empty here means the WHOLE preset must be refused (R4) — this is
    /// the batch-privilege-escalation channel, not a benign leftover key.
    pub stripped_org: Vec<String>,
    /// Every other per-agent-only field/table that was present and dropped.
    /// Non-empty here is a soft signal (authoring hygiene), not a refusal.
    pub stripped_other: Vec<String>,
}

/// Strip every field design §2.2 lists as "always per-agent" out of a raw
/// preset config table, classifying what was removed.
///
/// `[agent]` is dropped **entirely** (not just the four org fields) —
/// design §2.2 lists `display_name`/`trigger`/`icon`/`status` as per-agent
/// identity alongside `name`/`reports_to`/`department`, so the whole table is
/// identity, never job definition. Only the three `AGENT_ORG_FIELDS`
/// (`name`/`reports_to`/`department`) are escalated to `stripped_org`;
/// everything else in `[agent]` (and the other per-agent-only sections) is a
/// soft strip.
pub fn sanitize_preset_table(input: &toml::value::Table) -> SanitizeReport {
    let mut table = input.clone();
    let mut stripped_org = Vec::new();
    let mut stripped_other = Vec::new();

    if let Some(toml::Value::Table(agent_tbl)) = table.get(ORG_TABLE) {
        for field in AGENT_ORG_FIELDS {
            if let Some(v) = agent_tbl.get(*field) {
                if !toml_scalar_is_blank(v) {
                    stripped_org.push(format!("agent.{field}"));
                }
            }
        }
        stripped_org.sort();
        for key in agent_tbl.keys() {
            if !AGENT_ORG_FIELDS.contains(&key.as_str()) {
                stripped_other.push(format!("agent.{key}"));
            }
        }
    }
    if table.remove(ORG_TABLE).is_some() && stripped_org.is_empty() && stripped_other.is_empty() {
        // An empty `[agent]` table carried nothing worth reporting; still
        // removed, just not called out as a strip.
    }

    for name in SECRET_TABLES {
        if table.remove(*name).is_some() {
            stripped_other.push((*name).to_string());
        }
    }

    for (section, field) in PER_AGENT_SCALAR_FIELDS {
        if let Some(toml::Value::Table(sect)) = table.get_mut(*section) {
            if sect.remove(*field).is_some() {
                stripped_other.push(format!("{section}.{field}"));
            }
        }
    }

    SanitizeReport { table, stripped_org, stripped_other }
}

fn toml_scalar_is_blank(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

// ── Preset content ───────────────────────────────────────────────────────

/// `[preset]` metadata block inside a `preset.toml` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetMeta {
    pub id: String,
    pub version: String,
    pub label: String,
    pub description: String,
}

/// A preset file, parsed + sanitized + hashed.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPreset {
    pub meta: PresetMeta,
    /// The config table with every per-agent-only field already stripped.
    pub config: toml::value::Table,
    /// SHA-256 (hex) of the canonical serialization of `config` — the pin
    /// stored in a binding, and what drift detection compares against.
    pub content_sha256: String,
    pub stripped_org: Vec<String>,
    pub stripped_other: Vec<String>,
}

/// Why a preset file could not be loaded / resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetError {
    NotFound(String),
    Invalid(String),
    OrgFieldsRejected(Vec<String>),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::NotFound(id) => write!(f, "preset '{id}' not found"),
            PresetError::Invalid(reason) => write!(f, "preset content invalid: {reason}"),
            PresetError::OrgFieldsRejected(fields) => write!(
                f,
                "preset carries org-authority fields, refused: {}",
                fields.join(", ")
            ),
        }
    }
}

/// Parse + sanitize a preset file already read into a string. Split out from
/// [`load_preset`] so tests can exercise it without touching disk.
pub fn parse_preset(id: &str, text: &str) -> Result<ParsedPreset, PresetError> {
    let table: toml::value::Table =
        text.parse::<toml::Table>().map_err(|e| PresetError::Invalid(e.to_string()))?;

    let preset_tbl = table.get("preset").and_then(|v| v.as_table());
    let field = |k: &str| preset_tbl.and_then(|t| t.get(k)).and_then(|v| v.as_str()).unwrap_or("");
    let meta = PresetMeta {
        id: id.to_string(),
        version: {
            let v = field("version");
            if v.is_empty() { "0.0.0".to_string() } else { v.to_string() }
        },
        label: field("label").to_string(),
        description: field("description").to_string(),
    };

    // Everything except the `[preset]` metadata block itself is config.
    let mut config_table = table;
    config_table.remove("preset");

    let report = sanitize_preset_table(&config_table);
    if !report.stripped_org.is_empty() {
        return Err(PresetError::OrgFieldsRejected(report.stripped_org));
    }

    let content_sha256 = hash_table(&report.table);
    Ok(ParsedPreset {
        meta,
        config: report.table,
        content_sha256,
        stripped_org: report.stripped_org,
        stripped_other: report.stripped_other,
    })
}

/// Canonical SHA-256 (hex) of a TOML table's re-serialization. Hashing the
/// canonical form (not the raw file bytes) means comment-only / whitespace
/// edits to a preset file do not register as content drift — only the
/// resolved values matter.
pub fn hash_table(table: &toml::value::Table) -> String {
    let canonical = toml::to_string(table).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Load `<home>/presets/<id>/preset.toml`. `Err(NotFound)` for a missing
/// file or directory; parse/sanitize errors propagate as-is.
pub fn load_preset(home: &Path, id: &str) -> Result<ParsedPreset, PresetError> {
    let path = preset_file_path(home, id);
    let text = std::fs::read_to_string(&path).map_err(|_| PresetError::NotFound(id.to_string()))?;
    parse_preset(id, &text)
}

/// List preset ids present under `<home>/presets/`, sorted. A missing
/// directory yields an empty list, not an error.
pub fn list_presets(home: &Path) -> Vec<String> {
    let dir = presets_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && e.path().join(PRESET_FILE).is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

// ── Binding store (authority) ───────────────────────────────────────────

/// One agent's current preset binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetBinding {
    pub preset_id: String,
    pub version: String,
    pub content_sha256: String,
    pub bound_at: String,
    pub bound_by: String,
    pub reason: String,
}

/// The whole binding store, keyed by agent **directory name** (matching
/// `org_store`'s convention — the delegation/registry namespace, not
/// `[agent] name`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetBindingStore {
    entries: BTreeMap<String, PresetBinding>,
}

impl PresetBindingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, agent_id: &str) -> Option<&PresetBinding> {
        self.entries.get(agent_id.trim())
    }

    pub fn contains(&self, agent_id: &str) -> bool {
        self.get(agent_id).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PresetBinding)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn set(&mut self, agent_id: impl Into<String>, binding: PresetBinding) {
        let key = agent_id.into().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.entries.insert(key, binding);
    }

    pub fn remove(&mut self, agent_id: &str) -> Option<PresetBinding> {
        self.entries.remove(agent_id.trim())
    }
}

/// Read the binding store, never failing. Missing / unreadable / invalid /
/// future-schema all resolve to an empty store — every agent then resolves
/// as `Unbound`, which is the safe direction (an agent whose binding store
/// cannot be read boots with its own `agent.toml`, never with a guessed
/// preset).
pub fn load_bindings(home: &Path) -> PresetBindingStore {
    let path = bindings_path(home);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return PresetBindingStore::new(),
    };
    parse_bindings(&content).unwrap_or_default()
}

fn parse_bindings(content: &str) -> Result<PresetBindingStore, String> {
    let table = content.parse::<toml::Table>().map_err(|e| e.to_string())?;
    let schema = table.get("schema").and_then(|v| v.as_integer()).unwrap_or(PRESET_BINDINGS_SCHEMA);
    if schema > PRESET_BINDINGS_SCHEMA {
        return Err(format!("schema {schema} newer than this build understands"));
    }
    let mut store = PresetBindingStore::new();
    let Some(agents) = table.get("agents").and_then(|v| v.as_table()) else {
        return Ok(store);
    };
    for (agent_id, value) in agents {
        let Some(t) = value.as_table() else { continue };
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        store.set(
            agent_id.clone(),
            PresetBinding {
                preset_id: s("preset_id"),
                version: s("version"),
                content_sha256: s("content_sha256"),
                bound_at: s("bound_at"),
                bound_by: s("bound_by"),
                reason: s("reason"),
            },
        );
    }
    Ok(store)
}

fn serialize_bindings(store: &PresetBindingStore) -> String {
    let mut out = String::new();
    out.push_str(
        "# DuDuClaw preset 綁定權威資料（preset_bindings.toml）\n\
         #\n\
         # 這是「哪個 AI 員工目前套用哪個職務組合」的唯一權威來源。\n\
         # 各 AI 員工目錄下 agent.toml 的 [preset] 只是顯示用的鏡像，\n\
         # 改它不會改變實際套用結果；請用 `duduclaw preset bind` 調整。\n\n",
    );
    out.push_str(&format!("schema = {PRESET_BINDINGS_SCHEMA}\n"));
    for (agent_id, b) in store.iter() {
        out.push_str(&format!("\n[agents.{}]\n", toml_key(agent_id)));
        out.push_str(&format!("preset_id = {}\n", toml_string(&b.preset_id)));
        out.push_str(&format!("version = {}\n", toml_string(&b.version)));
        out.push_str(&format!("content_sha256 = {}\n", toml_string(&b.content_sha256)));
        out.push_str(&format!("bound_at = {}\n", toml_string(&b.bound_at)));
        out.push_str(&format!("bound_by = {}\n", toml_string(&b.bound_by)));
        out.push_str(&format!("reason = {}\n", toml_string(&b.reason)));
    }
    out
}

fn toml_key(key: &str) -> String {
    let bare_safe =
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare_safe { key.to_string() } else { toml_string(key) }
}

fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn mutate_bindings<T>(home: &Path, f: impl FnOnce(&mut PresetBindingStore) -> T) -> io::Result<T> {
    let path = bindings_path(home);
    std::fs::create_dir_all(home)?;
    with_file_lock(&path, || {
        let mut store = match std::fs::read_to_string(&path) {
            Ok(content) => match parse_bindings(&content) {
                Ok(s) => s,
                Err(reason) => {
                    let backup = path.with_extension("toml.corrupt");
                    let _ = std::fs::copy(&path, &backup);
                    tracing::warn!(
                        path = %path.display(),
                        backup = %backup.display(),
                        reason = %reason,
                        "preset_bindings.toml was not interpretable and is being rebuilt \
                         — the previous file was kept as a .corrupt copy"
                    );
                    PresetBindingStore::new()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => PresetBindingStore::new(),
            Err(e) => return Err(e),
        };
        let out = f(&mut store);
        write_atomic(&path, &serialize_bindings(&store))?;
        Ok(out)
    })
}

/// Append one binding-change record to the history JSONL. Best-effort: a
/// history-write failure is logged, never fails the binding operation
/// itself (the binding store write already committed).
fn append_history(home: &Path, entry: &serde_json::Value) {
    let path = bindings_history_path(home);
    let Ok(line) = serde_json::to_string(entry) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = with_file_lock(&path, || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{line}")
    });
}

/// Outcome of a successful [`bind`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindOutcome {
    pub binding: PresetBinding,
    pub previous: Option<PresetBinding>,
    /// Top-level `section.field` keys the agent's current `agent.toml`
    /// already overrides relative to the newly-bound preset (R1.4 "已覆寫").
    pub changed_fields: Vec<String>,
}

/// Bind `agent_id` to `preset_ref` (a preset id under `<home>/presets/`).
///
/// Validates the preset resolves and carries no org-authority fields before
/// writing anything (fail-closed: a bad preset never becomes a binding).
/// Writes the binding store and appends a history record. Does **not** write
/// the `agent.toml` mirror, an audit event, or an Activity Feed row — those
/// belong to callers that can depend on `duduclaw-security` /
/// `duduclaw-gateway` (`duduclaw-core` cannot).
pub fn bind(
    home: &Path,
    agent_id: &str,
    agent_dir: &Path,
    preset_ref: &str,
    actor: &str,
    reason: &str,
) -> Result<BindOutcome, PresetError> {
    let parsed = load_preset(home, preset_ref)?;

    let agent_table: toml::value::Table = std::fs::read_to_string(agent_dir.join("agent.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .unwrap_or_default();
    let changed_fields = diff_overridden_fields(&parsed.config, &agent_table);

    let now = chrono::Utc::now().to_rfc3339();
    let binding = PresetBinding {
        preset_id: parsed.meta.id.clone(),
        version: parsed.meta.version.clone(),
        content_sha256: parsed.content_sha256.clone(),
        bound_at: now.clone(),
        bound_by: actor.to_string(),
        reason: reason.to_string(),
    };

    let agent_key = agent_id.to_string();
    let binding_for_store = binding.clone();
    let previous = mutate_bindings(home, move |store| {
        let prev = store.get(&agent_key).cloned();
        store.set(agent_key, binding_for_store);
        prev
    })
    .map_err(|e| PresetError::Invalid(format!("寫入 preset_bindings.toml 失敗: {e}")))?;

    append_history(
        home,
        &serde_json::json!({
            "ts": now,
            "agent_id": agent_id,
            "action": "bind",
            "from": previous.as_ref().map(binding_json),
            "to": binding_json(&binding),
            "actor": actor,
            "reason": reason,
            "changed_fields": changed_fields,
        }),
    );

    Ok(BindOutcome { binding, previous, changed_fields })
}

/// Remove `agent_id`'s binding (back to fully per-agent `agent.toml`).
/// `Ok(None)` when there was nothing bound.
pub fn unbind(
    home: &Path,
    agent_id: &str,
    actor: &str,
    reason: &str,
) -> io::Result<Option<PresetBinding>> {
    let agent_key = agent_id.to_string();
    let removed = mutate_bindings(home, move |store| store.remove(&agent_key))?;
    if let Some(prev) = &removed {
        append_history(
            home,
            &serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "agent_id": agent_id,
                "action": "unbind",
                "from": binding_json(prev),
                "to": serde_json::Value::Null,
                "actor": actor,
                "reason": reason,
                "changed_fields": Vec::<String>::new(),
            }),
        );
    }
    Ok(removed)
}

fn binding_json(b: &PresetBinding) -> serde_json::Value {
    serde_json::json!({
        "preset_id": b.preset_id,
        "version": b.version,
        "content_sha256": b.content_sha256,
        "bound_at": b.bound_at,
        "bound_by": b.bound_by,
        "reason": b.reason,
    })
}

/// One-level `section.field` diff: keys present in BOTH the preset's
/// sanitized config and the agent's own raw `agent.toml` table, where the
/// agent's value differs — i.e., an explicit per-agent override (R1.4).
/// Deliberately shallow (one table level) — good enough for a dashboard
/// "已覆寫" badge; a byte-exact deep diff is not needed for P1.
pub fn diff_overridden_fields(
    preset_config: &toml::value::Table,
    agent_table: &toml::value::Table,
) -> Vec<String> {
    let mut out = Vec::new();
    for (section, preset_val) in preset_config {
        let Some(agent_val) = agent_table.get(section) else { continue };
        match (preset_val, agent_val) {
            (toml::Value::Table(pt), toml::Value::Table(at)) => {
                for (field, pv) in pt {
                    if let Some(av) = at.get(field) {
                        if av != pv {
                            out.push(format!("{section}.{field}"));
                        }
                    }
                }
            }
            _ => {
                if preset_val != agent_val {
                    out.push(section.clone());
                }
            }
        }
    }
    out.sort();
    out
}

// ── Resolution (design §1, §3) ──────────────────────────────────────────

/// The outcome of resolving one agent's preset binding against its current
/// `agent.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PresetResolution {
    /// No `preset_bindings.toml` entry for this agent — unchanged behaviour.
    #[default]
    Unbound,
    /// Binding resolved and merged successfully.
    Applied {
        preset_id: String,
        version: String,
        label: String,
        content_sha256: String,
        changed_fields: Vec<String>,
    },
    /// A binding exists but could not be safely applied — the agent boots
    /// with its own `agent.toml` only (R1.5 fail-closed), never a guess.
    Unresolved { preset_id: String, version: String, reason: String },
}

impl PresetResolution {
    pub fn is_applied(&self) -> bool {
        matches!(self, PresetResolution::Applied { .. })
    }
}

/// Resolve `agent_id`'s preset binding (if any) against its raw `agent.toml`
/// table, returning the table to actually load from (merged, or unchanged)
/// plus the resolution outcome.
///
/// This is the **single** resolution choke point — `duduclaw-agent`'s
/// `AgentRegistry::load_agent` and `duduclaw-gateway`'s dynamic-tail preset
/// line both call it, so a binding is described identically everywhere.
pub fn resolve_for_agent(
    home: &Path,
    agent_id: &str,
    agent_table: &toml::value::Table,
) -> (toml::value::Table, PresetResolution) {
    let Some(binding) = load_bindings(home).get(agent_id).cloned() else {
        return (agent_table.clone(), PresetResolution::Unbound);
    };

    let parsed = match load_preset(home, &binding.preset_id) {
        Ok(p) => p,
        Err(e) => {
            return (
                agent_table.clone(),
                PresetResolution::Unresolved {
                    preset_id: binding.preset_id,
                    version: binding.version,
                    reason: e.to_string(),
                },
            );
        }
    };

    if parsed.meta.version != binding.version {
        let reason = format!(
            "版本不符（綁定 {}，目前檔案 {}）",
            binding.version, parsed.meta.version
        );
        return (
            agent_table.clone(),
            PresetResolution::Unresolved { preset_id: binding.preset_id, version: binding.version, reason },
        );
    }
    if parsed.content_sha256 != binding.content_sha256 {
        return (
            agent_table.clone(),
            PresetResolution::Unresolved {
                preset_id: binding.preset_id,
                version: binding.version,
                reason: "preset 內容雜湊不符（檔案已變更但未重新綁定）".to_string(),
            },
        );
    }

    let changed_fields = diff_overridden_fields(&parsed.config, agent_table);
    let mut merged = parsed.config.clone();
    crate::toml_merge::merge_toml(&mut merged, agent_table);

    (
        merged,
        PresetResolution::Applied {
            preset_id: binding.preset_id,
            version: binding.version,
            label: parsed.meta.label,
            content_sha256: parsed.content_sha256,
            changed_fields,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_preset(home: &Path, id: &str, body: &str) {
        let dir = preset_dir(home, id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PRESET_FILE), body).unwrap();
    }

    const SALES_PRESET: &str = r#"
[preset]
version = "1.0.0"
label = "業務跟進助理"
description = "sales-followup department kit"

[model]
preferred = "claude-haiku-4-5"
fallback = "claude-sonnet-4-6"

[container]
timeout_ms = 120000
max_concurrent = 2

[capabilities]
allowed_tools = []
denied_tools = []
"#;

    #[test]
    fn sanitize_strips_agent_table_and_classifies_org_vs_other() {
        let table: toml::value::Table = r#"
[agent]
name = "dept-sales"
reports_to = "boss"
department = "sales"
display_name = "業務"
icon = "x"

[channels.discord]
bot_token = "secret"

[os_watch]
paths = ["~/x"]
ignore = ["*.log"]

[model]
account_pool = ["main"]
preferred = "claude-haiku-4-5"

[permissions]
can_modify_own_soul = true
can_create_agents = false
"#
        .parse()
        .unwrap();

        let report = sanitize_preset_table(&table);
        assert!(!report.table.contains_key("agent"));
        assert!(!report.table.contains_key("channels"));
        assert_eq!(
            report
                .table
                .get("os_watch")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("paths")),
            None,
            "paths must be stripped"
        );
        assert!(
            report
                .table
                .get("os_watch")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("ignore"))
                .is_some(),
            "ignore must survive"
        );
        assert_eq!(
            report.table.get("model").and_then(|v| v.as_table()).and_then(|t| t.get("account_pool")),
            None
        );
        assert_eq!(
            report
                .table
                .get("model")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("preferred"))
                .and_then(|v| v.as_str()),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            report
                .table
                .get("permissions")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("can_modify_own_soul")),
            None
        );

        assert_eq!(
            report.stripped_org,
            vec!["agent.department", "agent.name", "agent.reports_to"]
        );
        assert!(report.stripped_other.contains(&"channels".to_string()));
        assert!(report.stripped_other.contains(&"os_watch.paths".to_string()));
        assert!(report.stripped_other.contains(&"model.account_pool".to_string()));
        assert!(report.stripped_other.contains(&"permissions.can_modify_own_soul".to_string()));
    }

    #[test]
    fn sanitize_is_a_noop_for_a_well_formed_preset() {
        let table: toml::value::Table = SALES_PRESET.parse().unwrap();
        let mut config = table;
        config.remove("preset");
        let report = sanitize_preset_table(&config);
        assert!(report.stripped_org.is_empty());
        assert!(report.stripped_other.is_empty());
    }

    #[test]
    fn parse_preset_extracts_meta_and_hashes_content() {
        let p = parse_preset("sales-followup", SALES_PRESET).unwrap();
        assert_eq!(p.meta.id, "sales-followup");
        assert_eq!(p.meta.version, "1.0.0");
        assert_eq!(p.meta.label, "業務跟進助理");
        assert!(!p.config.contains_key("preset"), "metadata must not leak into config");
        assert_eq!(p.content_sha256.len(), 64);
    }

    #[test]
    fn parse_preset_rejects_org_fields() {
        let text = format!("{SALES_PRESET}\n[agent]\nreports_to = \"new-boss\"\n");
        let err = parse_preset("evil", &text).unwrap_err();
        assert!(matches!(err, PresetError::OrgFieldsRejected(_)));
    }

    #[test]
    fn missing_preset_is_not_found() {
        let h = home();
        let err = load_preset(h.path(), "nope").unwrap_err();
        assert!(matches!(err, PresetError::NotFound(_)));
    }

    #[test]
    fn list_presets_is_sorted_and_empty_for_missing_dir() {
        let h = home();
        assert!(list_presets(h.path()).is_empty());
        write_preset(h.path(), "zzz", SALES_PRESET);
        write_preset(h.path(), "aaa", SALES_PRESET);
        assert_eq!(list_presets(h.path()), vec!["aaa".to_string(), "zzz".to_string()]);
    }

    fn write_agent(home: &Path, id: &str, body: &str) -> PathBuf {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
        dir
    }

    const MINIMAL_AGENT: &str = r#"
[agent]
name = "clinic-sales"
display_name = "Clinic Sales"
role = "worker"
status = "active"
trigger = "@clinic-sales"
reports_to = "clinic-boss"
icon = "x"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = true
gvu_enabled = false
max_silence_hours = 168.0
max_gvu_generations = 0
observation_period_hours = 24.0
skill_token_budget = 500
max_active_skills = 2
"#;

    #[test]
    fn unbound_agent_resolves_to_the_original_table_byte_identical() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (resolved, resolution) = resolve_for_agent(h.path(), "clinic-sales", &table);
        assert_eq!(resolution, PresetResolution::Unbound);
        assert_eq!(resolved, table);
    }

    #[test]
    fn bind_then_resolve_merges_preset_under_per_agent_overrides() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        write_preset(h.path(), "sales-followup", SALES_PRESET);

        let outcome = bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();
        assert_eq!(outcome.binding.preset_id, "sales-followup");
        assert_eq!(outcome.binding.version, "1.0.0");
        // The agent's own [model] preferred/fallback override the preset's.
        assert!(outcome.changed_fields.contains(&"model.preferred".to_string()));
        assert!(outcome.changed_fields.contains(&"model.fallback".to_string()));

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (merged, resolution) = resolve_for_agent(h.path(), "clinic-sales", &table);
        let PresetResolution::Applied { preset_id, changed_fields, .. } = resolution else {
            panic!("expected Applied, got something else");
        };
        assert_eq!(preset_id, "sales-followup");
        assert!(changed_fields.contains(&"model.preferred".to_string()));

        // The agent's own `[container]` table is present in agent.toml, so
        // ITS value wins over the preset's (R1.2/R1.4: per-agent always
        // beats preset for keys the agent actually wrote).
        assert_eq!(
            merged.get("container").and_then(|v| v.as_table()).and_then(|t| t.get("timeout_ms")),
            Some(&toml::Value::Integer(60000)),
            "per-agent value must win over the preset's"
        );
        // Per-agent override wins over the preset's value.
        assert_eq!(
            merged.get("model").and_then(|v| v.as_table()).and_then(|t| t.get("preferred")).and_then(|v| v.as_str()),
            Some("claude-sonnet-4-6")
        );
        // `[capabilities]` is absent from the agent's own agent.toml, so it
        // must be inherited from the preset untouched — proves a
        // preset-only section really does flow through the merge.
        assert!(
            merged.get("capabilities").and_then(|v| v.as_table()).and_then(|t| t.get("allowed_tools")).is_some(),
            "a section the agent never wrote must be inherited from the preset"
        );
    }

    #[test]
    fn per_agent_array_write_clears_rather_than_merges_with_preset() {
        let h = home();
        let agent_with_tools = format!(
            "{MINIMAL_AGENT}\n[capabilities]\nallowed_tools = []\ndenied_tools = [\"Bash\"]\n"
        );
        let dir = write_agent(h.path(), "clinic-sales", &agent_with_tools);
        let preset_with_tools = SALES_PRESET.replace(
            "[capabilities]\nallowed_tools = []\ndenied_tools = []",
            "[capabilities]\nallowed_tools = [\"Bash\", \"WebFetch\"]\ndenied_tools = []",
        );
        write_preset(h.path(), "sales-followup", &preset_with_tools);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (merged, _) = resolve_for_agent(h.path(), "clinic-sales", &table);
        let caps = merged.get("capabilities").and_then(|v| v.as_table()).unwrap();
        // The agent explicitly wrote `allowed_tools = []` — that must WIN
        // over the preset's non-empty list, not merge with it (R1.3).
        assert_eq!(caps.get("allowed_tools").and_then(|v| v.as_array()).map(|a| a.len()), Some(0));
        // denied_tools: the agent wrote its own list, preset's (empty) must
        // not clear it.
        assert_eq!(
            caps.get("denied_tools").and_then(|v| v.as_array()).and_then(|a| a[0].as_str()),
            Some("Bash")
        );
    }

    #[test]
    fn bind_rejects_a_preset_carrying_org_fields_before_writing_anything() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        let evil = format!("{SALES_PRESET}\n[agent]\nreports_to = \"attacker\"\n");
        write_preset(h.path(), "evil", &evil);

        let err = bind(h.path(), "clinic-sales", &dir, "evil", "tester", "test").unwrap_err();
        assert!(matches!(err, PresetError::OrgFieldsRejected(_)));
        assert!(!bindings_path(h.path()).exists(), "must not write a binding for a rejected preset");
    }

    #[test]
    fn bind_of_unknown_preset_id_errors_without_writing() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        let err = bind(h.path(), "clinic-sales", &dir, "nonexistent", "tester", "test").unwrap_err();
        assert!(matches!(err, PresetError::NotFound(_)));
        assert!(!bindings_path(h.path()).exists());
    }

    #[test]
    fn resolution_is_unresolved_fail_closed_when_preset_file_disappears() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        write_preset(h.path(), "sales-followup", SALES_PRESET);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();

        // Simulate the preset being deleted out from under a live binding.
        std::fs::remove_dir_all(preset_dir(h.path(), "sales-followup")).unwrap();

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (resolved, resolution) = resolve_for_agent(h.path(), "clinic-sales", &table);
        assert!(matches!(resolution, PresetResolution::Unresolved { .. }));
        // Fail-closed: falls back to the agent's own table, not a guess.
        assert_eq!(resolved, table);
    }

    #[test]
    fn resolution_is_unresolved_when_content_hash_drifts_without_a_rebind() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        write_preset(h.path(), "sales-followup", SALES_PRESET);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();

        // Someone hand-edits the preset file's config (not just a comment)
        // without bumping [preset] version — the bound hash no longer
        // matches.
        let edited = SALES_PRESET.replace("claude-haiku-4-5", "claude-opus-4-6");
        write_preset(h.path(), "sales-followup", &edited);

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (_, resolution) = resolve_for_agent(h.path(), "clinic-sales", &table);
        assert!(matches!(resolution, PresetResolution::Unresolved { .. }));
    }

    #[test]
    fn version_bump_without_rebind_is_also_unresolved() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        write_preset(h.path(), "sales-followup", SALES_PRESET);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();

        let bumped = SALES_PRESET.replace("version = \"1.0.0\"", "version = \"1.1.0\"");
        write_preset(h.path(), "sales-followup", &bumped);

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (_, resolution) = resolve_for_agent(h.path(), "clinic-sales", &table);
        match resolution {
            PresetResolution::Unresolved { reason, .. } => assert!(reason.contains("版本不符")),
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn unbind_removes_the_entry_and_records_history() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        write_preset(h.path(), "sales-followup", SALES_PRESET);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();

        let removed = unbind(h.path(), "clinic-sales", "tester", "cleanup").unwrap();
        assert!(removed.is_some());
        assert!(load_bindings(h.path()).get("clinic-sales").is_none());
        let history = std::fs::read_to_string(bindings_history_path(h.path())).unwrap();
        assert_eq!(history.lines().count(), 2, "one bind + one unbind entry");
    }

    #[test]
    fn agent_home_dir_only_resolves_the_standard_layout() {
        let h = home();
        let standard = h.path().join("agents").join("bob");
        assert_eq!(agent_home_dir(&standard), Some(h.path().to_path_buf()));

        let ephemeral = h.path().join("agents").join(".ephemeral").join("eph-1");
        assert_eq!(agent_home_dir(&ephemeral), None);
    }

    #[test]
    fn corrupt_bindings_file_is_fail_safe_on_read_and_backed_up_on_write() {
        let h = home();
        let dir = write_agent(h.path(), "clinic-sales", MINIMAL_AGENT);
        std::fs::write(bindings_path(h.path()), "not [ toml").unwrap();
        assert!(load_bindings(h.path()).is_empty());

        write_preset(h.path(), "sales-followup", SALES_PRESET);
        bind(h.path(), "clinic-sales", &dir, "sales-followup", "tester", "test").unwrap();
        assert_eq!(load_bindings(h.path()).len(), 1);
        assert!(bindings_path(h.path()).with_extension("toml.corrupt").exists());
    }

    #[test]
    fn future_schema_bindings_are_refused_rather_than_half_interpreted() {
        let h = home();
        std::fs::write(
            bindings_path(h.path()),
            "schema = 99\n[agents.a]\npreset_id = \"x\"\n",
        )
        .unwrap();
        assert!(load_bindings(h.path()).is_empty());
    }

    // ── Free built-in preset (system-operator) — P2 ─────────────────────

    /// The embedded `templates/presets/system-operator/preset.toml` must
    /// parse cleanly through the exact production path (`parse_preset`),
    /// carry `system_operator = true` + `autonomy_level = "operator"` under
    /// `[capabilities]`, and strip nothing (it deliberately carries no
    /// `[agent]` table — the sanitizer would strip it silently, but a
    /// well-authored preset shouldn't have one to begin with).
    #[test]
    fn system_operator_preset_toml_parses_with_expected_capabilities() {
        let parsed = parse_preset(SYSTEM_OPERATOR_PRESET_ID, SYSTEM_OPERATOR_PRESET_TOML).unwrap();
        assert_eq!(parsed.meta.id, SYSTEM_OPERATOR_PRESET_ID);
        assert_eq!(parsed.meta.label, "系統操作員");
        assert!(parsed.stripped_org.is_empty());
        assert!(parsed.stripped_other.is_empty(), "{:?}", parsed.stripped_other);

        let caps = parsed.config.get("capabilities").and_then(|v| v.as_table()).expect("[capabilities]");
        assert_eq!(caps.get("system_operator"), Some(&toml::Value::Boolean(true)));
        assert_eq!(
            caps.get("autonomy_level").and_then(|v| v.as_str()),
            Some("operator"),
            "os_operator.rs's own module docs recommend autonomy_level = \"operator\" \
             for a system-operator persona"
        );
    }

    /// `install_system_operator_preset` writes the embedded content to
    /// `<home>/presets/system-operator/preset.toml`, is a no-op on a second
    /// call without `force` (existing file wins), and overwrites with
    /// `force: true` — same shape as `cmd_preset_install_builtin`'s existing
    /// skip/force semantics for the premium kits.
    #[test]
    fn install_system_operator_preset_is_idempotent_unless_forced() {
        let h = home();
        assert!(install_system_operator_preset(h.path(), false).unwrap(), "first install must write");
        let dest = preset_file_path(h.path(), SYSTEM_OPERATOR_PRESET_ID);
        assert!(dest.is_file());
        let original = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(original, SYSTEM_OPERATOR_PRESET_TOML);

        // Simulate a local hand-edit, then a non-forced re-install must not
        // touch it.
        std::fs::write(&dest, "# hand-edited\n").unwrap();
        assert!(!install_system_operator_preset(h.path(), false).unwrap(), "must skip an existing file");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# hand-edited\n");

        // `force: true` overwrites back to the embedded content.
        assert!(install_system_operator_preset(h.path(), true).unwrap());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), SYSTEM_OPERATOR_PRESET_TOML);
    }

    /// The installed preset is immediately usable through the real
    /// `bind` → `resolve_for_agent` pipeline — end-to-end proof that
    /// `agent create --preset system-operator` (which calls exactly this
    /// pipeline) lands `system_operator = true` + `autonomy_level =
    /// "operator"` in the agent's resolved config, without the caller
    /// hand-editing `agent.toml`.
    #[test]
    fn bound_system_operator_preset_materializes_capability_into_resolved_config() {
        let h = home();
        assert!(install_system_operator_preset(h.path(), false).unwrap());
        let dir = write_agent(h.path(), "front-desk-bot", MINIMAL_AGENT);

        let outcome =
            bind(h.path(), "front-desk-bot", &dir, SYSTEM_OPERATOR_PRESET_ID, "tester", "test").unwrap();
        assert_eq!(outcome.binding.preset_id, SYSTEM_OPERATOR_PRESET_ID);

        let table: toml::value::Table =
            std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
        let (merged, resolution) = resolve_for_agent(h.path(), "front-desk-bot", &table);
        assert!(resolution.is_applied());

        let caps = merged.get("capabilities").and_then(|v| v.as_table()).expect("[capabilities] must be inherited from the preset — MINIMAL_AGENT has none of its own");
        assert_eq!(caps.get("system_operator"), Some(&toml::Value::Boolean(true)));
        assert_eq!(caps.get("autonomy_level").and_then(|v| v.as_str()), Some("operator"));

        // O-0/O-4's own gates (Admin scope / is_appliance() / confirm /
        // ApprovalBroker) live entirely outside this table and are untouched
        // by a preset bind — nothing here can widen them, only the module
        // this crate owns (which fields a resolved agent.toml carries).
    }

    /// `builtin_soul_template` is the one lookup `cmd_agent_create` uses to
    /// decide whether to override the generic default SOUL.md — must return
    /// the embedded persona for the known free id and `None` for everything
    /// else (typos, premium preset ids, empty string).
    #[test]
    fn builtin_soul_template_only_matches_the_known_free_preset_id() {
        let body = builtin_soul_template(SYSTEM_OPERATOR_PRESET_ID).expect("must have a template");
        assert!(body.contains("系統操作員"));
        assert!(builtin_soul_template("sales-followup").is_none());
        assert!(builtin_soul_template("system-operator-typo").is_none());
        assert!(builtin_soul_template("").is_none());
    }

    /// WP-6F P1 item ⑤ — design §4.2 Step A: the 9 shared department kits
    /// under `commercial/templates-premium/teams/_departments/<kit>/agent.toml`
    /// become `commercial/templates-premium/presets/<kit>/preset.toml`,
    /// stripping the per-agent-only `[agent]` table (design §2.2) and
    /// leaving everything else untouched ("零語意損失").
    ///
    /// `#[ignore]`d: this WRITES real files under `commercial/` (a gitignored
    /// nested repo, may not exist in every checkout — e.g. open-source-only
    /// clones) and is meant to be re-run deliberately
    /// (`cargo test -p duduclaw-core --lib preset::tests::convert_department_kits_to_presets -- --ignored`)
    /// whenever a kit's `agent.toml` changes, matching the design's own
    /// framing for the 22-industry pass in P3 ("由既有 team.toml 腳本化產生,
    /// 不手寫") — this is that same script, scoped to P1's 9 kits.
    #[test]
    #[ignore = "writes real files under commercial/ — run deliberately, see doc comment"]
    fn convert_department_kits_to_presets() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir.join("../..");
        let premium_dir = repo_root.join("commercial/templates-premium");
        let departments_dir = premium_dir.join("teams/_departments");
        let presets_out_dir = premium_dir.join("presets");

        let Ok(entries) = std::fs::read_dir(&departments_dir) else {
            panic!(
                "department kits dir not found at {} — this test must run from a checkout \
                 that has the (gitignored) commercial/ tree",
                departments_dir.display()
            );
        };

        let mut converted = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let kit_id = entry.file_name().to_str().unwrap().to_string();
            let src = entry.path().join("agent.toml");
            if !src.is_file() {
                continue;
            }
            let raw = std::fs::read_to_string(&src).unwrap();
            let table: toml::value::Table = raw.parse().unwrap_or_else(|e| {
                panic!("{} does not parse as TOML: {e}", src.display())
            });

            // The label comes from the kit's own [agent] display_name —
            // read BEFORE sanitizing (sanitize strips the whole table).
            let label = table
                .get("agent")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("display_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(&kit_id)
                .to_string();

            // Kit files intentionally ship placeholder org-field values
            // (`name = "dept-sales"`, `reports_to = "CHANGE-ME"`, …) — the
            // header comment in every kit says as much ("部署時改…"). The
            // sanitizer strips the whole `[agent]` table regardless (design
            // §2.2), so this is the mechanical part of Step A doing its job,
            // not a defect to assert against.
            let report = sanitize_preset_table(&table);

            let mut out = String::new();
            out.push_str(&format!(
                "# DuDuClaw 職務組合（preset）——由 `commercial/templates-premium/teams/_departments/{kit_id}/agent.toml`\n\
                 # 機械轉換而來（design §4.2 Step A：移除 [agent] 身分欄位，其餘原樣搬入）。\n\
                 # 重新產生：cargo test -p duduclaw-core --lib preset::tests::convert_department_kits_to_presets -- --ignored\n\n"
            ));
            out.push_str("[preset]\n");
            out.push_str("version = \"1.0.0\"\n");
            out.push_str(&format!("label = {}\n", toml_string(&label)));
            out.push_str(&format!(
                "description = {}\n\n",
                toml_string(&format!("共用部門職務組合：{label}（{kit_id}）"))
            ));
            out.push_str(&toml::to_string_pretty(&report.table).unwrap());

            let dest_dir = presets_out_dir.join(&kit_id);
            std::fs::create_dir_all(&dest_dir).unwrap();
            std::fs::write(dest_dir.join(PRESET_FILE), &out).unwrap();

            // Round-trip check: the file this test just wrote must parse back
            // through the real production path with zero stripped fields
            // (proving the conversion didn't leave anything the sanitizer
            // would still object to) and a stable, hashable config.
            let parsed = parse_preset(&kit_id, &out).unwrap_or_else(|e| {
                panic!("{kit_id}: generated preset.toml failed to parse back: {e}")
            });
            assert!(parsed.stripped_org.is_empty());
            assert!(parsed.stripped_other.is_empty(), "{kit_id}: {:?}", parsed.stripped_other);
            converted.push(kit_id);
        }

        assert_eq!(converted.len(), 9, "expected exactly 9 department kits, converted: {converted:?}");
    }
}
