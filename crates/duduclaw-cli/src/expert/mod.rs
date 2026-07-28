//! `duduclaw expert` — expert-pack install / pack / list / remove / export
//! (WP2.1 + WP2.2).
//!
//! An *expert pack* is a portable bundle of a team (agents + `reports_to`
//! hierarchy), its skills, wiki SOPs, recommended prompts and channel hints.
//! The native format is [`manifest`] (`expert.toml`); two P0 importers ingest
//! foreign formats fail-closed:
//!
//! - **Claude Code plugin** (`.claude-plugin/plugin.json`) — [`plugin`].
//! - **Agent Skills single skill** (`SKILL.md`) — [`skill_import`].
//!
//! Security is fail-closed throughout: zip extraction is fenced against
//! zip-slip with a 50 MB cap ([`safe_zip`]); every foreign SOUL/SKILL body is
//! demoted to DATA and scanned by the prompt-injection guard **and** the skill
//! security scanner before it can land ([`scan_external`]); imported hooks are
//! copied disabled and never wired; an unrecognised layout is rejected with a
//! contents listing.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use console::style;

use duduclaw_core::error::{DuDuClawError, Result};

pub mod detect;
pub mod hooks;
mod install;
pub mod manifest;
mod plugin;
mod safe_zip;
mod skill_import;
mod team_convert;
pub mod topo;

#[cfg(test)]
mod tests;

/// UI locale for display-name resolution.
const UI_LOCALE: &str = "zh-TW";

#[derive(Subcommand)]
pub enum ExpertCommands {
    /// Install an expert pack from a directory, `.zip`, or URL.
    ///
    /// Format is auto-detected (native `expert.toml` → Claude Code plugin →
    /// single Agent Skill); an unrecognised layout is rejected. Foreign
    /// personas/skills are scanned before landing; imported hooks are disabled.
    Install {
        /// Path to a pack directory, a `.zip`, or an `http(s)://…zip` URL.
        source: String,
        /// Preview the plan without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Import under a `-imported` suffix on agent-id clashes instead of
        /// reporting a conflict.
        #[arg(long)]
        rename: bool,
        /// Explicitly trust and enable the pack's hooks (the codex / claude
        /// plugin `--trust` convention). Without this flag hooks are imported
        /// disabled and an ApprovalBroker request is filed (fail-closed).
        #[arg(long)]
        trust_hooks: bool,
        /// Attach the pack's root agents (empty `reports_to`) under an
        /// existing agent — e.g. your CEO / front-desk supervisor. The target
        /// must already exist; a typo aborts before anything installs.
        #[arg(long)]
        attach_under: Option<String>,
    },

    /// Validate and package a pack directory into a distributable `.zip`.
    Pack {
        /// The pack directory (must contain `expert.toml`).
        dir: PathBuf,
        /// Output `.zip` path (default: `<slug>-<version>.zip`).
        #[arg(long)]
        out: Option<PathBuf>,
    },

    /// List installed expert packs.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Remove an installed expert pack (its agents, pack-owned skills, and
    /// wiki pages). Assets that pre-existed the install are left untouched.
    Remove {
        /// Pack slug (see `expert list`).
        slug: String,
    },

    /// Export an installed pack to another platform's format.
    Export {
        /// Pack slug (see `expert list`).
        slug: String,
        /// Target format. P0: `claude-plugin`.
        #[arg(long, default_value = "claude-plugin")]
        format: String,
        /// Output directory (default: `./<slug>-<format>`).
        #[arg(long)]
        out: Option<PathBuf>,
    },

    /// Show / apply the ApprovalBroker decision for a pack's imported hooks:
    /// approved → enable, denied / expired → keep disabled (fail-closed).
    Hooks {
        /// Pack slug (see `expert list`).
        slug: String,
    },

    /// Batch-convert legacy team playbooks (`teams/<industry>-team/`) into
    /// native expert packs (`expert.toml` + agents + skills + wiki SOP).
    /// Idempotent: output is deterministic from the sources; re-running
    /// overwrites with identical content.
    ConvertTeams {
        /// Directory containing `<industry>-team/` playbooks (each with a
        /// `team.toml`), e.g. `commercial/templates-premium/teams`.
        teams_dir: PathBuf,
        /// Output directory for generated packs
        /// (default: `<teams_dir>/../experts`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// CLI entry point.
pub async fn run(cmd: ExpertCommands) -> Result<()> {
    let home = crate::duduclaw_home();
    match cmd {
        ExpertCommands::Install {
            source,
            dry_run,
            rename,
            trust_hooks,
            attach_under,
        } => install::cmd_install(&home, &source, dry_run, rename, trust_hooks, attach_under).await,
        ExpertCommands::Pack { dir, out } => cmd_pack(&dir, out.as_deref()),
        ExpertCommands::List { json } => cmd_list(&home, json),
        ExpertCommands::Remove { slug } => cmd_remove(&home, &slug).await,
        ExpertCommands::Export { slug, format, out } => {
            install::cmd_export(&home, &slug, &format, out.as_deref()).await
        }
        ExpertCommands::Hooks { slug } => hooks::cmd_hooks(&home, &slug).await,
        ExpertCommands::ConvertTeams { teams_dir, out } => {
            team_convert::cmd_convert_teams(&teams_dir, out.as_deref())
        }
    }
}

// ─────────────────────────── Install record ───────────────────────────
//
// The on-disk contract (`~/.duduclaw/experts/<slug>/install.json`, the hooks
// state machine, and the remove semantics) is SHARED with the dashboard admin
// RPCs and lives in `duduclaw_gateway::expert_admin` (cli → gateway is the
// legal dependency direction). Re-exported here so the CLI and the dashboard
// can never drift.

pub use duduclaw_gateway::expert_admin::{
    InstallRecord, PackKind, experts_dir, list_records, read_record,
};

/// Persist an install record (atomic temp + rename). Thin wrapper mapping the
/// shared impl's `String` error into the CLI error type.
pub fn write_record(home: &Path, rec: &InstallRecord) -> Result<()> {
    duduclaw_gateway::expert_admin::write_record(home, rec).map_err(cfg_err)
}

// ─────────────────────────── Report ───────────────────────────

/// Status of one planned/applied item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemStatus {
    Imported,
    Skipped,
    Conflict,
    Warning,
    Ignored,
}

#[derive(Debug, Clone)]
struct Item {
    status: ItemStatus,
    kind: String,
    name: String,
    detail: String,
}

/// A running import report — honest, never silently drops.
#[derive(Debug, Default)]
struct Report {
    items: Vec<Item>,
}

impl Report {
    fn add(&mut self, status: ItemStatus, kind: &str, name: &str, detail: impl Into<String>) {
        self.items.push(Item {
            status,
            kind: kind.to_string(),
            name: name.to_string(),
            detail: detail.into(),
        });
    }
    fn imported(&mut self, kind: &str, name: &str) {
        self.add(ItemStatus::Imported, kind, name, "");
    }
    fn skipped(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Skipped, kind, name, detail);
    }
    fn conflict(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Conflict, kind, name, detail);
    }
    fn warning(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Warning, kind, name, detail);
    }
    fn ignored(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Ignored, kind, name, detail);
    }

    fn render_console(&self, dry_run: bool) {
        let header = if dry_run {
            "安裝計畫（--dry-run，未寫入）"
        } else {
            "安裝結果"
        };
        println!("\n  {}\n", style(header).bold());
        for it in &self.items {
            let (mark, tag) = match it.status {
                ItemStatus::Imported => (style("✓").green(), style("匯入").green()),
                ItemStatus::Skipped => (style("–").yellow(), style("略過").yellow()),
                ItemStatus::Conflict => (style("!").red(), style("衝突").red()),
                ItemStatus::Warning => (style("⚠").yellow(), style("警告").yellow()),
                ItemStatus::Ignored => (style("·").dim(), style("忽略").dim()),
            };
            let detail = if it.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", it.detail)
            };
            println!("  {mark} [{tag}] {}: {}{detail}", it.kind, it.name);
        }
        println!();
    }
}

// ─────────────────────────── Security ───────────────────────────

/// Result of scanning a foreign persona/skill body.
enum ScanVerdict {
    Ok,
    Blocked(String),
}

/// Demote foreign `content` to DATA and run it through both scanners:
/// the prompt-injection input-guard (`duduclaw-security`) and the skill
/// security scanner (`duduclaw-gateway::skill_lifecycle::security_scanner`).
/// Fail-closed: any block/high-risk finding stops this asset from landing.
fn scan_external(content: &str) -> ScanVerdict {
    let inj = duduclaw_security::input_guard::scan_input(
        content,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if inj.blocked {
        return ScanVerdict::Blocked(format!(
            "prompt-injection risk {} (rules: {})",
            inj.risk_score,
            inj.matched_rules.join("/")
        ));
    }
    let scan = duduclaw_gateway::skill_lifecycle::security_scanner::scan_skill(content, None);
    if !scan.passed {
        let cats: Vec<String> = scan
            .findings
            .iter()
            .map(|f| format!("{:?}", f.category))
            .collect();
        return ScanVerdict::Blocked(format!(
            "security scan {:?} (findings: {})",
            scan.risk_level,
            cats.join("/")
        ));
    }
    ScanVerdict::Ok
}

// ─────────────────────────── Shared helpers ───────────────────────────

/// Map a foreign short model alias to a DuDuClaw `[model] preferred` id.
/// Returns `(preferred, needs_review)`; `None` = inherit the scaffold default.
fn map_model(raw: &str) -> (Option<String>, bool) {
    match raw.trim() {
        "" | "inherit" => (None, false),
        "sonnet" => (Some("claude-sonnet-4-6".into()), false),
        "opus" => (Some("claude-opus-4-5".into()), false),
        "haiku" => (Some("claude-haiku-4-5".into()), false),
        other if other.starts_with("claude") => (Some(other.into()), false),
        // Any non-Claude id is kept verbatim but flagged for human review
        // (multi-model platform — never silently coerce to one model).
        other => (Some(other.into()), true),
    }
}

/// Deep-merge `overlay` into `base` (overlay wins on scalar clashes; tables
/// merge recursively). Used to fold `agent.partial.toml` onto the scaffold.
fn merge_toml(base: &mut toml::value::Table, overlay: &toml::value::Table) {
    for (k, v) in overlay {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(bt)), toml::Value::Table(ot)) => merge_toml(bt, ot),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Read `<home>/agents/<id>/agent.toml`, apply `mutate`, write back atomically.
fn patch_agent_toml(
    home: &Path,
    agent_id: &str,
    mutate: impl FnOnce(&mut toml::value::Table),
) -> Result<()> {
    let path = home.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", path.display())))?;
    let mut table: toml::value::Table = content
        .parse::<toml::Table>()
        .map_err(|e| cfg_err(format!("解析 agent.toml 失敗: {e}")))?;
    mutate(&mut table);
    let out = toml::to_string_pretty(&table)
        .map_err(|e| cfg_err(format!("序列化 agent.toml 失敗: {e}")))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out).map_err(|e| io_err(format!("寫入暫存檔失敗: {e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(format!("覆寫 agent.toml 失敗: {e}"))
    })?;
    Ok(())
}

/// Set `[capabilities] allowed_tools` / `denied_tools` on an agent.toml table.
fn set_capabilities(table: &mut toml::value::Table, allowed: &[String], denied: &[String]) {
    let cap = table
        .entry("capabilities")
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
    if let toml::Value::Table(ct) = cap {
        if !allowed.is_empty() {
            ct.insert(
                "allowed_tools".into(),
                toml::Value::Array(allowed.iter().cloned().map(toml::Value::String).collect()),
            );
        }
        if !denied.is_empty() {
            ct.insert(
                "denied_tools".into(),
                toml::Value::Array(denied.iter().cloned().map(toml::Value::String).collect()),
            );
        }
    }
}

// Recursive symlink-skipping dir copy + ISO timestamp — shared impls.
pub(crate) use duduclaw_gateway::expert_admin::{copy_dir, now_iso};

// ─────────────────────────── per-agent .mcp.json merge ───────────────────────────

/// The wired MCP server key the scaffold always writes; never overwritten by
/// imported pack servers (fail-safe — a pack cannot hijack the duduclaw tool
/// surface).
const WIRED_MCP_KEY: &str = "duduclaw";

/// Read a `.mcp.json` file's `mcpServers` object (JSON5-tolerant). `None` when
/// the file is absent / unparseable / has no `mcpServers`.
pub(super) fn read_mcp_servers(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = json5::from_str(&raw)
        .or_else(|_| serde_json::from_str(&raw))
        .ok()?;
    val.get("mcpServers").and_then(|v| v.as_object()).cloned()
}

/// Merge `extra` MCP servers into `doc`'s `mcpServers`, overwriting same-name
/// entries **except** [`WIRED_MCP_KEY`] (`duduclaw`), which is never touched.
/// Ensures `doc.mcpServers` exists. Returns the number of servers written.
/// Pure (no I/O) so it is directly unit-testable.
pub(super) fn merge_mcp_servers(
    doc: &mut serde_json::Value,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> usize {
    if !doc.is_object() {
        *doc = serde_json::json!({});
    }
    let obj = doc.as_object_mut().expect("just set to object");
    let servers_val = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !servers_val.is_object() {
        *servers_val = serde_json::Value::Object(serde_json::Map::new());
    }
    let servers = servers_val.as_object_mut().expect("just ensured object");
    let mut written = 0;
    for (k, v) in extra {
        if k == WIRED_MCP_KEY {
            continue; // never clobber the wired duduclaw server
        }
        servers.insert(k.clone(), v.clone());
        written += 1;
    }
    written
}

/// Merge `extra` MCP servers into `<home>/agents/<id>/.mcp.json` (the file the
/// scaffold already wrote with the duduclaw server). Preserves the duduclaw
/// entry. Returns the number of servers written. Used by both the native and
/// Claude-plugin importers.
pub(super) fn merge_agent_mcp(
    home: &Path,
    agent_id: &str,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> Result<usize> {
    let path = home.join("agents").join(agent_id).join(".mcp.json");
    let mut doc: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| {
            json5::from_str(&c)
                .or_else(|_| serde_json::from_str(&c))
                .ok()
        })
        .unwrap_or_else(|| serde_json::json!({ "mcpServers": {} }));
    let written = merge_mcp_servers(&mut doc, extra);
    let out = serde_json::to_string_pretty(&doc)
        .map_err(|e| cfg_err(format!("序列化 .mcp.json 失敗: {e}")))?;
    std::fs::write(&path, out).map_err(|e| io_err(format!("寫入 .mcp.json 失敗: {e}")))?;
    Ok(written)
}

fn io_err(msg: String) -> DuDuClawError {
    DuDuClawError::Io(std::io::Error::other(msg))
}
fn cfg_err(msg: String) -> DuDuClawError {
    DuDuClawError::Config(msg)
}

// ─────────────────────────── pack / list / remove ───────────────────────────

fn cmd_pack(dir: &Path, out: Option<&Path>) -> Result<()> {
    if !dir.join("expert.toml").is_file() {
        return Err(cfg_err(format!(
            "{} 沒有 expert.toml —— `pack` 只打包原生專家包",
            dir.display()
        )));
    }
    let m = manifest::read(dir).map_err(cfg_err)?;
    let problems = manifest::validate(&m, dir);
    if !problems.is_empty() {
        eprintln!("\n  {}\n", style("打包驗證失敗").red().bold());
        for p in &problems {
            eprintln!("  {} {}: {}", style("✗").red(), p.field, p.message);
        }
        eprintln!();
        return Err(cfg_err(format!("{} 項驗證問題", problems.len())));
    }

    let slug = m.expert.name.clone();
    let version = if m.expert.version.is_empty() {
        "0.0.0".to_string()
    } else {
        m.expert.version.clone()
    };
    let default_out = PathBuf::from(format!("{slug}-{version}.zip"));
    let out_path = out.unwrap_or(&default_out);
    let total = safe_zip::pack_dir(dir, out_path)?;

    println!(
        "\n  {} 已打包 {} v{} → {} ({} bytes)\n",
        style("✓").green(),
        style(&slug).bold(),
        version,
        out_path.display(),
        total
    );
    Ok(())
}

fn cmd_list(home: &Path, json: bool) -> Result<()> {
    let records = list_records(home);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records)
                .map_err(|e| cfg_err(format!("序列化失敗: {e}")))?
        );
        return Ok(());
    }
    if records.is_empty() {
        println!("\n  尚未安裝任何專家包。用 `duduclaw expert install <path|zip|url>` 安裝。\n");
        return Ok(());
    }
    println!("\n  {}\n", style("已安裝的專家包").bold());
    for r in &records {
        let name = if r.display_name.is_empty() {
            r.slug.clone()
        } else {
            format!("{} ({})", r.display_name, r.slug)
        };
        println!(
            "  {} {}  v{}  [{}]",
            style("●").cyan(),
            style(name).bold(),
            r.version,
            r.kind.label()
        );
        println!(
            "      agents: {} · skills: {} · wiki: {}",
            r.agents.len(),
            r.global_skills.len(),
            r.wiki_files.len()
        );
    }
    println!();
    Ok(())
}

async fn cmd_remove(home: &Path, slug: &str) -> Result<()> {
    // Shared impl (also behind the dashboard `experts.remove` RPC): removes
    // recorded agents / pack-owned skills / wiki pages (fenced under
    // shared/wiki), then the record dir itself.
    let items = duduclaw_gateway::expert_admin::remove_pack(home, slug)
        .await
        .map_err(cfg_err)?;

    let mut report = Report::default();
    for it in &items {
        match it.status {
            "removed" => report.imported(&format!("removed-{}", it.kind), &it.name),
            "missing" => report.ignored(it.kind, &it.name, it.detail.clone()),
            _ => report.skipped(it.kind, &it.name, it.detail.clone()),
        }
    }

    report.render_console(false);
    println!(
        "  {} 已移除專家包 {}\n",
        style("✓").green(),
        style(slug).bold()
    );
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn model_alias_mapping() {
        assert_eq!(
            map_model("sonnet"),
            (Some("claude-sonnet-4-6".into()), false)
        );
        assert_eq!(map_model("haiku"), (Some("claude-haiku-4-5".into()), false));
        assert_eq!(map_model(""), (None, false));
        assert_eq!(map_model("inherit"), (None, false));
        assert_eq!(
            map_model("claude-opus-4-8"),
            (Some("claude-opus-4-8".into()), false)
        );
        // Non-Claude → kept but flagged.
        let (id, review) = map_model("gpt-4o");
        assert_eq!(id.as_deref(), Some("gpt-4o"));
        assert!(review);
    }

    #[test]
    fn merge_toml_recursive() {
        let mut base: toml::value::Table = "[model]\npreferred = 'a'\nfallback = 'f'\n"
            .parse::<toml::Table>()
            .unwrap();
        let overlay: toml::value::Table =
            "[model]\npreferred = 'b'\n[budget]\nmonthly_limit_cents = 100\n"
                .parse::<toml::Table>()
                .unwrap();
        merge_toml(&mut base, &overlay);
        let model = base["model"].as_table().unwrap();
        assert_eq!(model["preferred"].as_str(), Some("b")); // overlay wins
        assert_eq!(model["fallback"].as_str(), Some("f")); // base kept
        assert!(base.contains_key("budget")); // new section added
    }

    #[test]
    fn set_capabilities_writes_arrays() {
        let mut t = toml::value::Table::new();
        set_capabilities(&mut t, &["Read".into(), "Bash".into()], &["Write".into()]);
        let cap = t["capabilities"].as_table().unwrap();
        assert_eq!(cap["allowed_tools"].as_array().unwrap().len(), 2);
        assert_eq!(cap["denied_tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_mcp_preserves_duduclaw_and_overwrites_others() {
        let mut doc = serde_json::json!({
            "mcpServers": {
                "duduclaw": { "command": "duduclaw" },
                "keep": { "command": "original" }
            }
        });
        let mut extra = serde_json::Map::new();
        // A hostile pack tries to hijack the wired duduclaw server …
        extra.insert(
            "duduclaw".into(),
            serde_json::json!({ "command": "HIJACK" }),
        );
        // … adds a new server …
        extra.insert(
            "photoshop".into(),
            serde_json::json!({ "command": "psmcp" }),
        );
        // … and overrides an existing non-duduclaw server.
        extra.insert(
            "keep".into(),
            serde_json::json!({ "command": "overwritten" }),
        );

        let written = merge_mcp_servers(&mut doc, &extra);

        let servers = doc["mcpServers"].as_object().unwrap();
        assert_eq!(
            servers["duduclaw"]["command"], "duduclaw",
            "duduclaw NEVER overwritten"
        );
        assert_eq!(
            servers["photoshop"]["command"], "psmcp",
            "pack server added"
        );
        assert_eq!(
            servers["keep"]["command"], "overwritten",
            "same-name (non-duduclaw) overwritten"
        );
        assert_eq!(written, 2, "photoshop + keep written; duduclaw skipped");
    }

    #[test]
    fn merge_mcp_creates_servers_object_when_absent() {
        let mut doc = serde_json::json!({});
        let mut extra = serde_json::Map::new();
        extra.insert("x".into(), serde_json::json!({ "command": "x" }));
        assert_eq!(merge_mcp_servers(&mut doc, &extra), 1);
        assert!(doc["mcpServers"]["x"].is_object());
    }
}
