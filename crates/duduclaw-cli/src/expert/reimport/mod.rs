//! Importing something that was imported before.
//!
//! Before this, a second import of the same pack always failed ("已經匯入
//! 過" / "已安裝"), and the only way forward was removing the first one by
//! hand. Now the operator picks one of two explicit options:
//!
//! - **replace** (`--replace <slug>`): the earlier import is moved to a
//!   backup first (employees into `agents/_trash/<id>_<stamp>`, the same
//!   place `agent_remove` uses; its skills, knowledge pages, original files
//!   and record into `experts/_replaced/<slug>_<stamp>/`), then the new
//!   version installs under the same names. If the new install fails, the
//!   backup is moved back. An employee edited after the import is never
//!   replaced silently: without `--accept-edits` the run stops with
//!   `import.replace_edited` and the list of what changed.
//! - **copy** (`--as-copy`): installs next to the earlier one, under a new
//!   import name (`<slug>-2`, …) with renamed employee ids.
//!
//! Without either flag the run stops with `import.already_imported`, whose
//! parameters tell the dashboard what it can offer.

pub mod baseline;
mod retire;

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;
use serde_json::json;

use duduclaw_core::error::Result;

use super::import_error::import_err;
use super::{InstallRecord, read_record};
use baseline::{ChangeKind, FileChange};

pub(crate) use retire::{finish, retire, rollback};

/// What the operator asked for when the import already exists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReimportMode {
    /// First import (a clash is an error).
    #[default]
    Fresh,
    /// Replace the earlier import named `slug`.
    Replace { slug: String, accept_edits: bool },
    /// Keep the earlier import and add this one as a copy.
    Copy,
}

/// The earlier import being replaced, resolved before anything is written.
#[derive(Debug, Clone)]
pub struct ReplaceTarget {
    pub record: InstallRecord,
    pub accept_edits: bool,
    /// Employee ids whose names become free for the new version.
    pub freed: BTreeSet<String>,
}

/// Per-employee changes since the import (what a replace would move away).
#[derive(Debug, Clone, Serialize)]
pub struct AgentEdits {
    pub agent: String,
    pub display: String,
    /// `"import"`: compared with the baseline taken at import time;
    /// `"time"`: older import without one, compared by modification time.
    pub baseline: &'static str,
    pub changes: Vec<FileChange>,
    /// Changes not listed (the list is capped).
    pub more: usize,
}

/// Files listed per employee; the rest are counted in `more`.
const MAX_LISTED_CHANGES: usize = 20;

/// Resolve `mode` against the home: for a replace, the target must exist.
pub fn resolve_target(home: &Path, mode: &ReimportMode) -> Result<Option<ReplaceTarget>> {
    let ReimportMode::Replace { slug, accept_edits } = mode else {
        return Ok(None);
    };
    if !crate::is_valid_agent_id(slug) {
        return Err(import_err(
            "import.replace_missing",
            json!({ "slug": duduclaw_core::truncate_chars(slug, 64) }),
            format!("要取代的匯入代號「{}」不合法", slug.escape_debug()),
        ));
    }
    let Some(record) = read_record(home, slug) else {
        return Err(import_err(
            "import.replace_missing",
            json!({ "slug": slug }),
            format!("找不到要取代的匯入「{slug}」（可能已經移除）"),
        ));
    };
    let freed = record.agents.iter().cloned().collect();
    Ok(Some(ReplaceTarget { record, accept_edits: *accept_edits, freed }))
}

/// Changes made to each employee of `rec` since it was imported. Employees
/// whose directory is already gone are left out (nothing to lose).
pub fn edits_since_import(home: &Path, rec: &InstallRecord) -> Vec<AgentEdits> {
    let base = baseline::read(home, &rec.slug);
    let mut out = Vec::new();
    for id in &rec.agents {
        if !crate::is_valid_agent_id(id) {
            continue;
        }
        let dir = home.join("agents").join(id);
        if !dir.is_dir() {
            continue;
        }
        let (kind, mut changes) = match base.as_ref().and_then(|b| b.agents.get(id)) {
            Some(before) => match baseline::fingerprint(&dir) {
                Some(now) => ("import", baseline::diff(before, &now)),
                None => ("import", vec![unknown_change()]),
            },
            None => ("time", baseline::changed_after(&dir, &rec.installed_at)),
        };
        if changes.is_empty() {
            continue;
        }
        let more = changes.len().saturating_sub(MAX_LISTED_CHANGES);
        changes.truncate(MAX_LISTED_CHANGES);
        out.push(AgentEdits {
            agent: id.clone(),
            display: display_name(&dir).unwrap_or_else(|| id.clone()),
            baseline: kind,
            changes,
            more,
        });
    }
    out
}

fn unknown_change() -> FileChange {
    FileChange { path: String::new(), change: ChangeKind::Unknown }
}

fn display_name(agent_dir: &Path) -> Option<String> {
    let table: toml::Table = std::fs::read_to_string(agent_dir.join("agent.toml")).ok()?.parse().ok()?;
    table
        .get("agent")?
        .get("display_name")?
        .as_str()
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// The slug this install may use, given what is already imported.
///
/// - nothing under `slug` → `slug` (a replace must then be naming it: the
///   real run has already moved the earlier record to the backup);
/// - replace of exactly `slug` → `slug`;
/// - copy → the first free `slug-2` … `slug-99`;
/// - otherwise → `import.already_imported`, with what the dashboard needs
///   to offer replace / copy (and whether employees were edited).
pub fn claim_slug(home: &Path, mode: &ReimportMode, slug: &str) -> Result<String> {
    if let ReimportMode::Replace { slug: target, .. } = mode
        && target != slug
    {
        return Err(import_err(
            "import.replace_mismatch",
            json!({ "expected": target, "actual": slug }),
            format!("這次上傳的內容是「{slug}」，和要取代的「{target}」不是同一份"),
        ));
    }
    let Some(existing) = read_record(home, slug) else {
        return Ok(slug.to_string());
    };
    match mode {
        ReimportMode::Replace { .. } => Ok(slug.to_string()),
        ReimportMode::Copy => (2..100)
            .map(|n| format!("{slug}-{n}"))
            .find(|s| crate::is_valid_agent_id(s) && read_record(home, s).is_none())
            .ok_or_else(|| {
                import_err(
                    "import.copy_exhausted",
                    json!({ "slug": slug }),
                    format!("「{slug}」已經有太多副本，請先移除不需要的再匯入"),
                )
            }),
        ReimportMode::Fresh => Err(already_imported(home, &existing)),
    }
}

fn already_imported(home: &Path, rec: &InstallRecord) -> duduclaw_core::error::DuDuClawError {
    let edits = edits_since_import(home, rec);
    let name = if rec.display_name.trim().is_empty() { rec.slug.clone() } else { rec.display_name.clone() };
    import_err(
        "import.already_imported",
        json!({
            "slug": rec.slug,
            "name": name,
            "kind": rec.kind.label(),
            "agents": rec.agents,
            "installed_at": rec.installed_at,
            "edited": !edits.is_empty(),
            "edits": edits,
        }),
        format!("「{name}」之前已經匯入過。可以選擇取代之前的版本，或另存一份副本"),
    )
}

/// A replace of employees edited since the import needs explicit consent.
/// Terminal-only follow-up for a failure code (the dashboard has buttons
/// for these instead; failure messages themselves never name a command).
pub fn cli_hint(code: &str, params: &serde_json::Value) -> Option<String> {
    let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("<代號>");
    match code {
        "import.already_imported" => Some(format!(
            "取代：加 --replace {slug}；另存副本：加 --as-copy；或先執行 duduclaw expert remove {slug}"
        )),
        "import.replace_edited" => Some("確定要取代（修改會移到備份）：再加 --accept-edits".to_string()),
        "import.nothing_imported" => Some("同名員工衝突時，可加 --as-copy 或 --rename".to_string()),
        _ => None,
    }
}

pub fn ensure_edits_accepted(home: &Path, target: &ReplaceTarget) -> Result<Vec<AgentEdits>> {
    let edits = edits_since_import(home, &target.record);
    if edits.is_empty() || target.accept_edits {
        return Ok(edits);
    }
    let names: Vec<&str> = edits.iter().map(|e| e.display.as_str()).collect();
    Err(import_err(
        "import.replace_edited",
        json!({ "slug": target.record.slug, "edits": edits }),
        format!(
            "{} 在匯入後被修改過；取代會把這些修改移到備份，需要再確認一次才會取代",
            names.join("、")
        ),
    ))
}

/// The `details.replace` object of the report: what a replace moves away.
pub fn replace_detail(target: &ReplaceTarget, edits: &[AgentEdits]) -> serde_json::Value {
    let rec = &target.record;
    json!({
        "slug": rec.slug,
        "name": if rec.display_name.trim().is_empty() { rec.slug.clone() } else { rec.display_name.clone() },
        "agents": rec.agents,
        "edited": !edits.is_empty(),
        "edits": edits,
    })
}

impl crate::expert::install::InstallCtx {
    /// Employee ids a replace is about to free (empty otherwise).
    fn freed(&self) -> Option<&BTreeSet<String>> {
        self.replace.as_ref().map(|t| &t.freed)
    }

    /// Is `id` used by an employee that stays? A replaced employee's id is
    /// free even in a dry run, where its directory still exists.
    pub fn agent_taken(&self, id: &str) -> bool {
        let freed = self.freed().map(|f| f.contains(id)).unwrap_or(false);
        !freed && self.home.join("agents").join(id).exists()
    }

    /// Names the remaining employees answer to.
    pub fn name_book(&self) -> duduclaw_core::agent_names::NameBook {
        match self.freed() {
            Some(skip) => duduclaw_core::agent_names::NameBook::load_excluding(&self.home, skip),
            None => duduclaw_core::agent_names::NameBook::load(&self.home),
        }
    }

    /// [`claim_slug`] for this run.
    pub fn claim_slug(&self, slug: &str) -> Result<String> {
        claim_slug(&self.home, &self.reimport, slug)
    }

    /// Refuse when adding `adding` employees would exceed the agent cap.
    pub async fn ensure_quota(&self, adding: usize) -> Result<()> {
        let freed = self
            .freed()
            .map(|f| {
                f.iter()
                    .filter(|id| self.home.join("agents").join(id.as_str()).join("agent.toml").is_file())
                    .count()
            })
            .unwrap_or(0);
        super::quota::ensure(&self.home, self.agent_cap, adding, freed).await
    }
}

#[cfg(test)]
mod tests;
