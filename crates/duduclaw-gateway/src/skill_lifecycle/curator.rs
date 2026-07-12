//! G5 curator lifecycle — usage-based skill maintenance.
//!
//! A deterministic background pass (hooked into the synthesis scheduler's
//! poll loop, gated by its own config + 24h guard) that walks every skill
//! file in the loader scan roots (`<home>/skills` and
//! `<home>/agents/<id>/SKILLS`) and applies the ClawHub-benchmark curation
//! rules:
//!
//! - unused ≥ `stale_days` (default 30) ⇒ **stale** (visible flag only)
//! - unused ≥ `archive_days` (default 90) ⇒ **archived**: the file is moved to
//!   `<home>/skills-archive/<scope>/…` (outside every loader scan root, so it
//!   drops out of prompts/skill lists) and remains recoverable — **but only
//!   for skills with a recorded usage history**. Skills with NO usage signal
//!   at all (normal/hub skills never routed through the `Skill` tool-use
//!   stamp) are stale-flagged (report-only) and never auto-archived: an
//!   absent signal is not evidence of disuse (fail-safe).
//! - `pinned` exempts a skill from both transitions
//! - a stale skill that gets used again is reactivated
//! - a skill whose on-disk artifact the curator cannot locate (e.g. a nested
//!   `group/<name>/SKILL.md` layout) is marked **unmanaged** once and skipped
//!   in later passes instead of erroring every tick
//!
//! The usage signal is `custom_skills.db`: `usage_count`/`last_used_at` on the
//! registry (stamped by the channel-reply stream-json path via
//! [`crate::custom_skills::CustomSkillStore::increment_usage_by_slug`]) plus
//! the per-file `skill_curation` rows. A skill the curator has never seen gets
//! `first_seen = now` — nothing is archived before 90 tracked days, so
//! enabling the curator on an existing install is safe.
//!
//! Every pass appends a maintenance report to the shared wiki at
//! `<home>/shared/wiki/reports/skill-pipeline/<YYYY-MM>/curator-<date>.md`
//! (the same `reports/skill-pipeline/` convention the repo wiki uses).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tracing::{info, warn};

#[cfg(test)]
use crate::custom_skills::SkillCurationRecord;
use crate::custom_skills::{CurationStatus, CustomSkillStore};

/// `curator_meta` key holding the RFC-3339 time of the last completed pass.
const META_LAST_RUN: &str = "curator_last_run_at";

/// Minimum spacing between scheduled passes (manual runs bypass this).
const RUN_INTERVAL_SECS: i64 = 24 * 3600;

/// Scope string for the global skills dir.
pub const SCOPE_GLOBAL: &str = "global";

// ── Config ──────────────────────────────────────────────────

/// Parsed `[skill_curator]` settings from `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct CuratorConfig {
    /// Master switch. Default **true** — the pass is deterministic, the
    /// first-seen baseline makes it non-destructive for 90 days, and pinning
    /// gives operators a permanent opt-out per skill.
    pub enabled: bool,
    /// Days without use before a skill is flagged stale (7..=365, default 30).
    pub stale_days: i64,
    /// Days without use before a skill is archived (> stale_days, ≤ 730,
    /// default 90).
    pub archive_days: i64,
    /// Report window: skills within this many days of going stale are listed
    /// as "approaching stale" (default 7).
    pub approaching_margin_days: i64,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_days: 30,
            archive_days: 90,
            approaching_margin_days: 7,
        }
    }
}

impl CuratorConfig {
    /// Parse `[skill_curator]` from raw `config.toml`. Malformed input falls
    /// back to defaults; out-of-range values are clamped.
    pub fn from_config_str(content: &str) -> Self {
        let mut cfg = Self::default();
        let table: toml::Value = match content.parse() {
            Ok(t) => t,
            Err(_) => return cfg,
        };
        let Some(s) = table.get("skill_curator") else {
            return cfg;
        };
        if let Some(v) = s.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = s.get("stale_days").and_then(|v| v.as_integer()) {
            cfg.stale_days = v.clamp(7, 365);
        }
        if let Some(v) = s.get("archive_days").and_then(|v| v.as_integer()) {
            cfg.archive_days = v.clamp(cfg.stale_days + 1, 730);
        }
        // Invariant even when only stale_days was customized upward.
        if cfg.archive_days <= cfg.stale_days {
            cfg.archive_days = (cfg.stale_days + 1).min(730);
        }
        cfg
    }

    pub fn load_from_home(home_dir: &Path) -> Self {
        match std::fs::read_to_string(home_dir.join("config.toml")) {
            Ok(c) => Self::from_config_str(&c),
            Err(_) => Self::default(),
        }
    }
}

// ── Pure decision function ──────────────────────────────────

/// What the pass should do with one skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurationAction {
    Keep,
    MarkStale,
    Archive,
    Reactivate,
}

/// Deterministic transition rule. `days_unused` counts from
/// `last_used_at.unwrap_or(first_seen)`.
///
/// - pinned ⇒ never stale/archive; a pinned stale row heals back to active
/// - archived rows are terminal here (recovery goes through the pin/restore
///   path, never the scheduled pass)
/// - unmanaged rows are exempt from scheduled transitions (flagged once)
/// - `has_usage_signal` = a real `last_used_at` was ever recorded (curation
///   row or registry stamp). Without it, `days_unused` counts from
///   `first_seen`, which says nothing about actual use — normal/hub skills
///   are never stamped by the `Skill` tool-use path, so archiving on that
///   clock would delete daily-used skills. Fail-safe: no signal ⇒ stale
///   flag only, never auto-archive.
pub fn decide(
    status: CurationStatus,
    pinned: bool,
    days_unused: i64,
    has_usage_signal: bool,
    cfg: &CuratorConfig,
) -> CurationAction {
    if status == CurationStatus::Archived || status == CurationStatus::Unmanaged {
        return CurationAction::Keep;
    }
    if pinned {
        return if status == CurationStatus::Stale {
            CurationAction::Reactivate
        } else {
            CurationAction::Keep
        };
    }
    if days_unused >= cfg.archive_days && has_usage_signal {
        return CurationAction::Archive;
    }
    match status {
        CurationStatus::Active if days_unused >= cfg.stale_days => CurationAction::MarkStale,
        CurationStatus::Stale if days_unused < cfg.stale_days => CurationAction::Reactivate,
        _ => CurationAction::Keep,
    }
}

// ── Filesystem helpers ──────────────────────────────────────

/// Skills dir for a scope string (`"global"` or `"agent:<id>"`). Returns None
/// for malformed scopes (fail-safe: no file action).
fn scope_dir(home_dir: &Path, scope: &str) -> Option<PathBuf> {
    if scope == SCOPE_GLOBAL {
        return Some(home_dir.join("skills"));
    }
    let id = scope.strip_prefix("agent:")?;
    if id.is_empty() || !is_safe_component(id) {
        return None;
    }
    Some(home_dir.join("agents").join(id).join("SKILLS"))
}

/// Directory-safe name check (mirrors the MCP-side validator: no traversal).
fn is_safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
        && s != "."
        && s != ".."
}

/// Locate the on-disk artifact for a skill: flat `<dir>/<name>.md` or the
/// Anthropic layout `<dir>/<name>/` (containing SKILL.md).
///
/// `name` originates from skill frontmatter (attacker-controlled DATA), so it
/// is validated as a single safe path component before any path is built —
/// a traversal name must never resolve to (and get archive-moved from) a
/// path outside the skills dir. Deeper layouts (`group/<name>/SKILL.md`)
/// are not managed here; callers mark them `unmanaged`.
fn locate_skill_artifact(dir: &Path, name: &str) -> Option<PathBuf> {
    if !duduclaw_agent::skill_loader::is_safe_skill_name(name) {
        return None;
    }
    let flat = dir.join(format!("{name}.md"));
    if flat.is_file() {
        return Some(flat);
    }
    let nested = dir.join(name);
    if nested.is_dir() && nested.join("SKILL.md").is_file() {
        return Some(nested);
    }
    None
}

/// Archive destination directory for a scope. `agent:<id>` ⇒ `agent-<id>`
/// (path-safe, non-ambiguous).
fn archive_dir(home_dir: &Path, scope: &str) -> PathBuf {
    let leaf = if scope == SCOPE_GLOBAL {
        SCOPE_GLOBAL.to_string()
    } else {
        format!("agent-{}", scope.strip_prefix("agent:").unwrap_or(scope))
    };
    home_dir.join("skills-archive").join(leaf)
}

/// Move a skill artifact into the archive tree. Returns the new path.
fn archive_artifact(home_dir: &Path, scope: &str, artifact: &Path) -> Result<PathBuf, String> {
    let dest_dir = archive_dir(home_dir, scope);
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("create archive dir: {e}"))?;
    let file_name = artifact
        .file_name()
        .ok_or_else(|| "artifact has no file name".to_string())?;
    let dest = dest_dir.join(file_name);
    if dest.exists() {
        return Err(format!(
            "archive destination already exists: {}",
            dest.display()
        ));
    }
    std::fs::rename(artifact, &dest).map_err(|e| format!("archive move: {e}"))?;
    Ok(dest)
}

// ── Discovery ───────────────────────────────────────────────

/// Enumerate `(skill_name, scope)` for every skill file in the loader scan
/// roots. Uses the same recursive loader the agents use, so nested layouts
/// are honoured.
async fn discover_skills(home_dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&home_dir.join("skills")).await {
        out.push((sk.name, SCOPE_GLOBAL.to_string()));
    }

    if let Ok(rd) = std::fs::read_dir(home_dir.join("agents")) {
        for entry in rd.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(agent_id) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if !is_safe_component(&agent_id) || agent_id.starts_with('_') {
                continue; // skip _trash/ and malformed dirs
            }
            let skills_dir = entry.path().join("SKILLS");
            for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&skills_dir).await {
                out.push((sk.name, format!("agent:{agent_id}")));
            }
        }
    }
    out
}

// ── Report ──────────────────────────────────────────────────

/// Outcome of one curator pass — everything the report and the MCP status
/// tool need.
#[derive(Debug, Default, serde::Serialize)]
pub struct CuratorReport {
    pub ran_at: String,
    pub tracked_total: usize,
    pub newly_tracked: Vec<String>,
    pub newly_stale: Vec<String>,
    pub newly_archived: Vec<String>,
    pub reactivated: Vec<String>,
    /// `(skill, days_unused)` for skills within the approaching-stale window.
    pub approaching_stale: Vec<(String, i64)>,
    pub removed_missing: Vec<String>,
    /// Artifacts the curator cannot locate/manage (nested layouts) — flagged
    /// once, then skipped by later passes.
    pub newly_unmanaged: Vec<String>,
    pub pinned_total: usize,
    pub stale_total: usize,
    pub archived_total: usize,
    /// Per-skill failures, never silently swallowed.
    pub errors: Vec<String>,
}

fn fmt_key(name: &str, scope: &str) -> String {
    format!("{name} [{scope}]")
}

/// Render the maintenance report block appended after each pass.
pub fn render_report(r: &CuratorReport, cfg: &CuratorConfig) -> String {
    fn section(title: &str, items: &[String]) -> String {
        if items.is_empty() {
            return String::new();
        }
        let body: String = items.iter().map(|i| format!("- {i}\n")).collect();
        format!("\n### {title} ({})\n{body}", items.len())
    }
    let approaching: Vec<String> = r
        .approaching_stale
        .iter()
        .map(|(k, d)| format!("{k} — {d} 天未使用（{} 天即轉 stale）", cfg.stale_days))
        .collect();
    format!(
        "## Curator pass — {}\n\n\
         追蹤中技能 {}｜stale {}｜archived {}｜pinned {}（stale 門檻 {} 天、archive 門檻 {} 天）\n{}{}{}{}{}{}{}{}",
        r.ran_at,
        r.tracked_total,
        r.stale_total,
        r.archived_total,
        r.pinned_total,
        cfg.stale_days,
        cfg.archive_days,
        section("新追蹤", &r.newly_tracked),
        section("新標記 stale", &r.newly_stale),
        section("已封存（可透過 skill_pin 復原）", &r.newly_archived),
        section("重新啟用", &r.reactivated),
        section("接近 stale", &approaching),
        section("檔案已消失，停止追蹤", &r.removed_missing),
        section("無法管理的巢狀配置（僅追蹤，不封存）", &r.newly_unmanaged),
        section("錯誤", &r.errors),
    )
}

/// Report file for a pass: shared-wiki `reports/skill-pipeline/<YYYY-MM>/`.
pub fn report_path(home_dir: &Path, now: DateTime<Utc>) -> PathBuf {
    home_dir
        .join("shared")
        .join("wiki")
        .join("reports")
        .join("skill-pipeline")
        .join(now.format("%Y-%m").to_string())
        .join(format!("curator-{}.md", now.format("%Y-%m-%d")))
}

fn append_report(home_dir: &Path, now: DateTime<Utc>, block: &str) -> Result<PathBuf, String> {
    let path = report_path(home_dir, now);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("report dir: {e}"))?;
    }
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    if !content.is_empty() {
        content.push_str("\n---\n\n");
    }
    content.push_str(block);
    content.push('\n');
    std::fs::write(&path, content).map_err(|e| format!("report write: {e}"))?;
    Ok(path)
}

// ── The pass ────────────────────────────────────────────────

/// Days between `from` (RFC-3339) and `now`. Unparseable ⇒ 0 (fail-safe:
/// treated as fresh, never triggers stale/archive on garbage data).
fn days_since(from: &str, now: DateTime<Utc>) -> i64 {
    match DateTime::parse_from_rfc3339(from) {
        Ok(dt) => (now - dt.with_timezone(&Utc)).num_days().max(0),
        Err(_) => 0,
    }
}

/// Run one curator pass. Deterministic given the DB + filesystem + `now`.
pub async fn run_pass(
    home_dir: &Path,
    store: &CustomSkillStore,
    cfg: &CuratorConfig,
    now: DateTime<Utc>,
) -> Result<CuratorReport, String> {
    let now_str = now.to_rfc3339();
    let mut report = CuratorReport {
        ran_at: now_str.clone(),
        ..Default::default()
    };

    // 1. Discover skill files and baseline new ones.
    let discovered = discover_skills(home_dir).await;
    let mut present: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for (name, scope) in &discovered {
        present.insert((name.clone(), scope.clone()));
        match store.curation_upsert_seen(name, scope, &now_str).await {
            Ok(true) => report.newly_tracked.push(fmt_key(name, scope)),
            Ok(false) => {}
            Err(e) => report
                .errors
                .push(format!("{}: track: {e}", fmt_key(name, scope))),
        }
    }

    // 2. Merge the registry usage signal (approved custom skills).
    let registry_last_used = store.registry_last_used_map().await.unwrap_or_default();

    // 3. Evaluate every tracked row.
    for rec in store.curation_list().await? {
        let key = fmt_key(&rec.skill_name, &rec.scope);

        // File vanished outside our control (and we didn't archive it) —
        // stop tracking rather than inventing state.
        if rec.status != CurationStatus::Archived
            && !present.contains(&(rec.skill_name.clone(), rec.scope.clone()))
        {
            match store.curation_remove(&rec.skill_name, &rec.scope).await {
                Ok(_) => report.removed_missing.push(key),
                Err(e) => report.errors.push(format!("{key}: untrack: {e}")),
            }
            continue;
        }

        let recorded_last_used = [
            rec.last_used_at.as_deref(),
            registry_last_used.get(&rec.skill_name).map(|s| s.as_str()),
        ]
        .into_iter()
        .flatten()
        .max() // RFC-3339 strings order lexicographically
        .map(|s| s.to_string());
        // No recorded usage AT ALL ⇒ no archive authority (fail-safe): the
        // clock then runs from first_seen, which only proves tracking age,
        // not disuse (normal/hub skills are never stamped by the Skill
        // tool-use path).
        let has_usage_signal = recorded_last_used.is_some();
        let effective_last_used =
            recorded_last_used.unwrap_or_else(|| rec.first_seen.clone());
        let days_unused = days_since(&effective_last_used, now);

        match decide(rec.status, rec.pinned, days_unused, has_usage_signal, cfg) {
            CurationAction::Keep => {
                if rec.status == CurationStatus::Active
                    && !rec.pinned
                    && days_unused >= cfg.stale_days - cfg.approaching_margin_days
                    && days_unused < cfg.stale_days
                {
                    report.approaching_stale.push((key, days_unused));
                }
            }
            CurationAction::MarkStale => {
                match store
                    .curation_set_status(
                        &rec.skill_name,
                        &rec.scope,
                        CurationStatus::Stale,
                        None,
                        &now_str,
                    )
                    .await
                {
                    Ok(_) => report.newly_stale.push(key),
                    Err(e) => report.errors.push(format!("{key}: mark stale: {e}")),
                }
            }
            CurationAction::Reactivate => {
                match store
                    .curation_set_status(
                        &rec.skill_name,
                        &rec.scope,
                        CurationStatus::Active,
                        None,
                        &now_str,
                    )
                    .await
                {
                    Ok(_) => report.reactivated.push(key),
                    Err(e) => report.errors.push(format!("{key}: reactivate: {e}")),
                }
            }
            CurationAction::Archive => {
                let Some(dir) = scope_dir(home_dir, &rec.scope) else {
                    report.errors.push(format!("{key}: malformed scope"));
                    continue;
                };
                let Some(artifact) = locate_skill_artifact(&dir, &rec.skill_name) else {
                    // Layout we can't manage (e.g. group/<name>/SKILL.md) —
                    // flag it unmanaged ONCE and skip it in later passes
                    // instead of logging the same error every tick.
                    match store
                        .curation_set_status(
                            &rec.skill_name,
                            &rec.scope,
                            CurationStatus::Unmanaged,
                            None,
                            &now_str,
                        )
                        .await
                    {
                        Ok(_) => report.newly_unmanaged.push(key),
                        Err(e) => report.errors.push(format!("{key}: mark unmanaged: {e}")),
                    }
                    continue;
                };
                match archive_artifact(home_dir, &rec.scope, &artifact) {
                    Ok(dest) => {
                        match store
                            .curation_set_status(
                                &rec.skill_name,
                                &rec.scope,
                                CurationStatus::Archived,
                                Some(&dest.to_string_lossy()),
                                &now_str,
                            )
                            .await
                        {
                            Ok(_) => report.newly_archived.push(key),
                            Err(e) => report.errors.push(format!("{key}: record archive: {e}")),
                        }
                    }
                    Err(e) => report.errors.push(format!("{key}: archive: {e}")),
                }
            }
        }
    }

    // 4. Totals (post-transition).
    for rec in store.curation_list().await? {
        if rec.pinned {
            report.pinned_total += 1;
        }
        match rec.status {
            CurationStatus::Stale => report.stale_total += 1,
            CurationStatus::Archived => report.archived_total += 1,
            CurationStatus::Active | CurationStatus::Unmanaged => {}
        }
    }
    report.tracked_total = store.curation_list().await?.len();

    // 5. Append the maintenance report.
    let block = render_report(&report, cfg);
    if let Err(e) = append_report(home_dir, now, &block) {
        warn!("curator report write failed: {e}");
        report.errors.push(format!("report: {e}"));
    }

    info!(
        tracked = report.tracked_total,
        stale = report.stale_total,
        archived = report.archived_total,
        errors = report.errors.len(),
        "skill curator pass complete"
    );
    Ok(report)
}

/// Scheduler entry point: run at most once per [`RUN_INTERVAL_SECS`], only
/// when `[skill_curator] enabled` (default true). Errors are logged, never
/// propagated into the scheduler loop.
pub async fn maybe_run(home_dir: &Path) {
    let cfg = CuratorConfig::load_from_home(home_dir);
    if !cfg.enabled {
        return;
    }
    let store = match CustomSkillStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!("curator: store open failed: {e}");
            return;
        }
    };
    let now = Utc::now();
    if let Ok(Some(last)) = store.meta_get(META_LAST_RUN).await {
        if let Ok(dt) = DateTime::parse_from_rfc3339(&last) {
            if (now - dt.with_timezone(&Utc)).num_seconds() < RUN_INTERVAL_SECS {
                return;
            }
        }
    }
    // Stamp BEFORE running so a crashing pass cannot hot-loop every poll.
    if let Err(e) = store.meta_set(META_LAST_RUN, &now.to_rfc3339()).await {
        warn!("curator: meta stamp failed: {e}");
        return;
    }
    if let Err(e) = run_pass(home_dir, &store, &cfg, now).await {
        warn!("curator pass failed: {e}");
    }
}

// ── Pin / restore (MCP-facing) ──────────────────────────────

/// Toggle a skill's pin flag. Pinning an **archived** skill also restores its
/// file from the archive back into the original skills dir (recoverability
/// contract). Returns a human-readable summary.
pub async fn set_pin(
    home_dir: &Path,
    store: &CustomSkillStore,
    skill_name: &str,
    scope: &str,
    pinned: bool,
) -> Result<String, String> {
    let now = Utc::now().to_rfc3339();
    let Some(rec) = store.curation_get(skill_name, scope).await? else {
        return Err(format!(
            "skill '{skill_name}' [{scope}] is not tracked by the curator (it appears after the next pass)"
        ));
    };

    if !store
        .curation_set_pinned(skill_name, scope, pinned, &now)
        .await?
    {
        return Err("pin update failed".to_string());
    }

    if pinned && rec.status == CurationStatus::Archived {
        // Restore the archived artifact.
        let archived = rec
            .archived_path
            .as_deref()
            .ok_or("archived skill has no recorded archive path")?;
        let archived = PathBuf::from(archived);
        if !archived.exists() {
            return Err(format!("archived file missing: {}", archived.display()));
        }
        let dir = scope_dir(home_dir, scope).ok_or("malformed scope")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("restore dir: {e}"))?;
        let file_name = archived
            .file_name()
            .ok_or("archive path has no file name")?;
        let dest = dir.join(file_name);
        if dest.exists() {
            return Err(format!(
                "restore destination already exists: {}",
                dest.display()
            ));
        }
        std::fs::rename(&archived, &dest).map_err(|e| format!("restore move: {e}"))?;
        store
            .curation_set_status(skill_name, scope, CurationStatus::Active, None, &now)
            .await?;
        return Ok(format!(
            "Skill '{skill_name}' [{scope}] pinned and restored from archive to {}",
            dest.display()
        ));
    }

    Ok(format!(
        "Skill '{skill_name}' [{scope}] {}",
        if pinned {
            "pinned (exempt from stale/archive)"
        } else {
            "unpinned"
        }
    ))
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CuratorConfig {
        CuratorConfig::default()
    }

    // ── decide(): the full transition table ─────────────────

    #[test]
    fn decide_stale_archive_pin_transitions() {
        use CurationAction::*;
        use CurationStatus::*;
        let c = cfg();

        // Fresh skill stays.
        assert_eq!(decide(Active, false, 0, true, &c), Keep);
        assert_eq!(decide(Active, false, 29, true, &c), Keep);
        // 30 days ⇒ stale.
        assert_eq!(decide(Active, false, 30, true, &c), MarkStale);
        assert_eq!(decide(Active, false, 89, true, &c), MarkStale);
        // 90 days ⇒ archive (from active or stale) — WITH a usage history.
        assert_eq!(decide(Active, false, 90, true, &c), Archive);
        assert_eq!(decide(Stale, false, 90, true, &c), Archive);
        assert_eq!(decide(Stale, false, 400, true, &c), Archive);
        // Stale + recent use ⇒ reactivate.
        assert_eq!(decide(Stale, false, 3, true, &c), Reactivate);
        // Stale within window stays stale.
        assert_eq!(decide(Stale, false, 45, true, &c), Keep);

        // Pin exempts from BOTH transitions.
        assert_eq!(decide(Active, true, 30, true, &c), Keep);
        assert_eq!(decide(Active, true, 5000, true, &c), Keep);
        assert_eq!(
            decide(Stale, true, 5000, true, &c),
            Reactivate,
            "pinning heals stale"
        );

        // Archived is terminal for the scheduled pass (recovery = pin path).
        assert_eq!(decide(Archived, false, 0, true, &c), Keep);
        assert_eq!(decide(Archived, true, 0, true, &c), Keep);
    }

    #[test]
    fn decide_never_archives_without_usage_signal() {
        // Fail-safe archive policy: no recorded usage AT ALL ⇒ stale flag
        // only, never a destructive file move — normal/hub skills are never
        // stamped by the Skill tool-use path, so first_seen age is not
        // evidence of disuse.
        use CurationAction::*;
        use CurationStatus::*;
        let c = cfg();
        assert_eq!(decide(Active, false, 90, false, &c), MarkStale);
        assert_eq!(decide(Active, false, 5000, false, &c), MarkStale);
        assert_eq!(decide(Stale, false, 5000, false, &c), Keep, "report-only");
        // Unmanaged rows are exempt from every scheduled transition.
        assert_eq!(decide(Unmanaged, false, 5000, true, &c), Keep);
        assert_eq!(decide(Unmanaged, false, 5000, false, &c), Keep);
    }

    #[test]
    fn config_parsing_clamps_and_fails_safe() {
        let d = CuratorConfig::default();
        assert!(d.enabled);
        assert_eq!((d.stale_days, d.archive_days), (30, 90));

        let c = CuratorConfig::from_config_str(
            "[skill_curator]\nenabled = false\nstale_days = 2\narchive_days = 9999\n",
        );
        assert!(!c.enabled);
        assert_eq!(c.stale_days, 7, "stale floor");
        assert_eq!(c.archive_days, 730, "archive ceiling");

        // archive must stay strictly above stale.
        let c = CuratorConfig::from_config_str(
            "[skill_curator]\nstale_days = 100\narchive_days = 50\n",
        );
        assert!(c.archive_days > c.stale_days);

        assert_eq!(
            CuratorConfig::from_config_str("garbage {{{"),
            CuratorConfig::default()
        );
    }

    #[test]
    fn scope_dir_rejects_traversal() {
        let home = Path::new("/tmp/home");
        assert!(scope_dir(home, "global").is_some());
        assert!(scope_dir(home, "agent:alice").is_some());
        assert!(scope_dir(home, "agent:").is_none());
        assert!(scope_dir(home, "agent:../evil").is_none());
        assert!(scope_dir(home, "agent:a/b").is_none());
        assert!(scope_dir(home, "bogus").is_none());
    }

    // ── Full pass over a synthetic home (synthetic timestamps) ──

    #[tokio::test(flavor = "current_thread")]
    async fn pass_marks_stale_archives_and_respects_pin() {
        let home = std::env::temp_dir().join(format!("duduclaw-curator-{}", uuid::Uuid::new_v4()));
        let skills = home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        for name in ["fresh-skill", "old-skill", "ancient-skill", "pinned-skill"] {
            std::fs::write(
                skills.join(format!("{name}.md")),
                format!("---\nname: {name}\n---\nbody"),
            )
            .unwrap();
        }

        let store = CustomSkillStore::open(&home).unwrap();
        let now = Utc::now();
        let days = |d: i64| (now - chrono::Duration::days(d)).to_rfc3339();
        let seed = |name: &str, first_seen_days: i64, last_used_days: Option<i64>, pinned: bool| {
            SkillCurationRecord {
                skill_name: name.to_string(),
                scope: SCOPE_GLOBAL.to_string(),
                first_seen: days(first_seen_days),
                last_used_at: last_used_days.map(days),
                pinned,
                status: CurationStatus::Active,
                archived_path: None,
                updated_at: days(first_seen_days),
            }
        };
        store
            .curation_put(&seed("fresh-skill", 200, Some(2), false))
            .await
            .unwrap();
        store
            .curation_put(&seed("old-skill", 200, Some(40), false))
            .await
            .unwrap();
        store
            .curation_put(&seed("ancient-skill", 200, Some(120), false))
            .await
            .unwrap();
        store
            .curation_put(&seed("pinned-skill", 200, Some(120), true))
            .await
            .unwrap();

        let report = run_pass(&home, &store, &cfg(), now).await.unwrap();

        assert!(
            report.errors.is_empty(),
            "no errors expected: {:?}",
            report.errors
        );
        assert_eq!(report.newly_stale, vec!["old-skill [global]"]);
        assert_eq!(report.newly_archived, vec!["ancient-skill [global]"]);

        // Stale is a flag; the file stays loadable.
        assert!(skills.join("old-skill.md").is_file());
        assert_eq!(
            store
                .curation_get("old-skill", "global")
                .await
                .unwrap()
                .unwrap()
                .status,
            CurationStatus::Stale
        );

        // Archived file moved out of the loader root, recoverable.
        assert!(!skills.join("ancient-skill.md").exists());
        let arch = store
            .curation_get("ancient-skill", "global")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(arch.status, CurationStatus::Archived);
        let arch_path = PathBuf::from(arch.archived_path.clone().unwrap());
        assert!(
            arch_path.is_file(),
            "archived file must exist at recorded path"
        );
        assert!(arch_path.starts_with(home.join("skills-archive")));

        // Pinned skill untouched despite 120 days idle.
        assert!(skills.join("pinned-skill.md").is_file());
        assert_eq!(
            store
                .curation_get("pinned-skill", "global")
                .await
                .unwrap()
                .unwrap()
                .status,
            CurationStatus::Active
        );

        // Report file appended under the shared wiki skill-pipeline tree.
        let rp = report_path(&home, now);
        let content = std::fs::read_to_string(&rp).unwrap();
        assert!(content.contains("old-skill"));
        assert!(content.contains("ancient-skill"));

        // Second pass is idempotent: nothing new happens.
        let report2 = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert!(report2.newly_stale.is_empty());
        assert!(report2.newly_archived.is_empty());
        assert!(report2.errors.is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_sighting_baselines_instead_of_archiving() {
        let home = std::env::temp_dir().join(format!("duduclaw-curator-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("skills")).unwrap();
        std::fs::write(
            home.join("skills").join("brand-new.md"),
            "---\nname: brand-new\n---\n",
        )
        .unwrap();

        let store = CustomSkillStore::open(&home).unwrap();
        let report = run_pass(&home, &store, &cfg(), Utc::now()).await.unwrap();

        assert_eq!(report.newly_tracked, vec!["brand-new [global]"]);
        assert!(report.newly_stale.is_empty());
        assert!(report.newly_archived.is_empty());
        assert!(home.join("skills").join("brand-new.md").is_file());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn never_used_skill_is_stale_flagged_but_never_archived() {
        let home = std::env::temp_dir().join(format!("duduclaw-curator-{}", uuid::Uuid::new_v4()));
        let skills = home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("daily-driver.md"),
            "---\nname: daily-driver\n---\nbody",
        )
        .unwrap();

        let store = CustomSkillStore::open(&home).unwrap();
        let now = Utc::now();
        // Tracked for 400 days, zero usage stamps (normal/hub skills never
        // get one) — MUST stay on disk, flagged stale only.
        store
            .curation_put(&SkillCurationRecord {
                skill_name: "daily-driver".into(),
                scope: SCOPE_GLOBAL.into(),
                first_seen: (now - chrono::Duration::days(400)).to_rfc3339(),
                last_used_at: None,
                pinned: false,
                status: CurationStatus::Active,
                archived_path: None,
                updated_at: now.to_rfc3339(),
            })
            .await
            .unwrap();

        let report = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert_eq!(report.newly_stale, vec!["daily-driver [global]"]);
        assert!(report.newly_archived.is_empty(), "no-signal ⇒ never archived");
        assert!(
            skills.join("daily-driver.md").is_file(),
            "file must stay in the loader root"
        );

        // Later passes keep it stale, still never archive.
        let report2 = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert!(report2.newly_archived.is_empty());
        assert!(skills.join("daily-driver.md").is_file());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlocatable_layout_is_marked_unmanaged_once() {
        let home = std::env::temp_dir().join(format!("duduclaw-curator-{}", uuid::Uuid::new_v4()));
        let skills = home.join("skills");
        // Nested `group/foo/SKILL.md` layout the curator cannot manage.
        std::fs::create_dir_all(skills.join("group").join("foo")).unwrap();
        std::fs::write(
            skills.join("group").join("foo").join("SKILL.md"),
            "---\nname: foo\n---\nbody",
        )
        .unwrap();

        let store = CustomSkillStore::open(&home).unwrap();
        let now = Utc::now();
        // Old + used-long-ago ⇒ archive-eligible, but the artifact is not
        // locatable at <dir>/foo.md or <dir>/foo/SKILL.md.
        store
            .curation_put(&SkillCurationRecord {
                skill_name: "foo".into(),
                scope: SCOPE_GLOBAL.into(),
                first_seen: (now - chrono::Duration::days(400)).to_rfc3339(),
                last_used_at: Some((now - chrono::Duration::days(200)).to_rfc3339()),
                pinned: false,
                status: CurationStatus::Active,
                archived_path: None,
                updated_at: now.to_rfc3339(),
            })
            .await
            .unwrap();

        let report = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert_eq!(report.newly_unmanaged, vec!["foo [global]"]);
        assert!(
            report.errors.is_empty(),
            "unlocatable layout is a flag, not an error: {:?}",
            report.errors
        );
        assert_eq!(
            store.curation_get("foo", SCOPE_GLOBAL).await.unwrap().unwrap().status,
            CurationStatus::Unmanaged
        );

        // Second pass: silent — no repeated flagging, no error spam.
        let report2 = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert!(report2.newly_unmanaged.is_empty());
        assert!(report2.errors.is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn locate_rejects_traversal_names() {
        // Frontmatter-derived names must never build paths outside the dir
        // (same class as the install-sink fix in skill_loader.rs).
        let dir = Path::new("/tmp/skills");
        assert!(locate_skill_artifact(dir, "../../agents/victim/SOUL").is_none());
        assert!(locate_skill_artifact(dir, "a/b").is_none());
        assert!(locate_skill_artifact(dir, ".hidden").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pin_restores_archived_skill() {
        let home = std::env::temp_dir().join(format!("duduclaw-curator-{}", uuid::Uuid::new_v4()));
        let skills = home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("dusty.md"), "---\nname: dusty\n---\n").unwrap();

        let store = CustomSkillStore::open(&home).unwrap();
        let now = Utc::now();
        store
            .curation_put(&SkillCurationRecord {
                skill_name: "dusty".into(),
                scope: SCOPE_GLOBAL.into(),
                first_seen: (now - chrono::Duration::days(400)).to_rfc3339(),
                // Was `None` — but the archive policy now requires a recorded
                // usage history (no-signal skills are stale-only, fail-safe),
                // so this test seeds a real, ancient last-used stamp.
                last_used_at: Some((now - chrono::Duration::days(400)).to_rfc3339()),
                pinned: false,
                status: CurationStatus::Active,
                archived_path: None,
                updated_at: now.to_rfc3339(),
            })
            .await
            .unwrap();

        // Pass archives it (used once, 400 days ago).
        let report = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert_eq!(report.newly_archived, vec!["dusty [global]"]);
        assert!(!skills.join("dusty.md").exists());

        // Pinning restores it and heals status.
        let msg = set_pin(&home, &store, "dusty", SCOPE_GLOBAL, true)
            .await
            .unwrap();
        assert!(msg.contains("restored"), "{msg}");
        assert!(
            skills.join("dusty.md").is_file(),
            "file restored into loader root"
        );
        let rec = store
            .curation_get("dusty", SCOPE_GLOBAL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.status, CurationStatus::Active);
        assert!(rec.pinned);
        assert!(rec.archived_path.is_none());

        // And future passes keep it (pinned).
        let report2 = run_pass(&home, &store, &cfg(), now).await.unwrap();
        assert!(report2.newly_archived.is_empty());
        assert!(skills.join("dusty.md").is_file());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn report_renders_all_sections() {
        let r = CuratorReport {
            ran_at: "2026-07-11T00:00:00Z".into(),
            tracked_total: 4,
            newly_tracked: vec!["a [global]".into()],
            newly_stale: vec!["b [global]".into()],
            newly_archived: vec!["c [agent:x]".into()],
            reactivated: vec![],
            approaching_stale: vec![("d [global]".into(), 25)],
            removed_missing: vec![],
            newly_unmanaged: vec!["e [global]".into()],
            pinned_total: 1,
            stale_total: 1,
            archived_total: 1,
            errors: vec![],
        };
        let md = render_report(&r, &CuratorConfig::default());
        assert!(md.contains("Curator pass"));
        assert!(md.contains("新標記 stale"));
        assert!(md.contains("已封存"));
        assert!(md.contains("接近 stale"));
        assert!(md.contains("d [global] — 25 天未使用"));
        assert!(md.contains("無法管理的巢狀配置"));
        // Empty sections are omitted.
        assert!(!md.contains("重新啟用"));
    }
}
