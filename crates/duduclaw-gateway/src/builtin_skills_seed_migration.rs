//! WP19 one-time migration — backfill the bundled skills for installs that
//! never got them.
//!
//! **Why this exists.** Every new AI staffer is supposed to start with the
//! bundled document skills (docx / xlsx / pptx / pdf). Up to v1.51.0 the
//! seeding call was wired to exactly one of the five agent-creation paths —
//! the MCP `create_agent` tool. Anyone who onboarded through the dashboard,
//! `duduclaw onboard`, `duduclaw agent create`, or the industry wizard got an
//! empty `SKILLS/`; and since nothing ever writes `<home>/skills/` either,
//! their Skills page was blank. The customer report "the skill library shows
//! nothing" was literally true and had nothing to do with the page's read path.
//!
//! Wiring the remaining four creation paths (v1.51.1) fixes agents created
//! *from now on*. It does nothing for the staffer already sitting in a
//! customer's `~/.duduclaw` — hence this migration.
//!
//! **Where it seeds.** `<home>/skills/`, the company-wide layer, not each
//! agent's `SKILLS/`. One write instead of N, every existing and future
//! staffer picks it up through `compose_skill_layers`, and it matches what the
//! skills actually are: shared capability, not one employee's private notes.
//!
//! **Safety.** Runs once (marker file), never overwrites an existing
//! `<name>/SKILL.md` (operator edits win), and never blocks boot: every failure
//! degrades to a `warn!`. The marker is the load-bearing part — without it an
//! operator who deliberately deletes a bundled skill would find it resurrected
//! on the next restart, forever.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Marker filename under `<home>/migrations/`. Its presence — regardless of
/// contents — suppresses re-runs.
const MARKER_NAME: &str = "wp19-builtin-skills-seed.done";

/// Outcome of one migration pass (returned for tests + boot logging).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Skills actually written by this pass.
    pub seeded: Vec<String>,
    /// True when a marker already existed, so nothing was written.
    pub already_done: bool,
    /// Populated when seeding itself failed (the pass still returns Ok).
    pub error: Option<String>,
}

fn marker_path(home_dir: &Path) -> PathBuf {
    home_dir.join("migrations").join(MARKER_NAME)
}

/// Run the migration if it has not run before.
///
/// Never returns an error: a wedged migration must not stop the gateway from
/// booting. Callers get a [`SeedReport`] for logging/tests.
pub fn run(home_dir: &Path) -> SeedReport {
    let mut report = SeedReport::default();
    let marker = marker_path(home_dir);
    if marker.exists() {
        report.already_done = true;
        return report;
    }

    let skills_dir = home_dir.join("skills");
    match duduclaw_agent::builtin_skills::install_builtin_skills(&skills_dir) {
        Ok(names) => {
            report.seeded = names.iter().map(|n| n.to_string()).collect();
            if report.seeded.is_empty() {
                info!(
                    dir = %skills_dir.display(),
                    "WP19 migration: bundled skills already present — nothing to backfill"
                );
            } else {
                info!(
                    dir = %skills_dir.display(),
                    count = report.seeded.len(),
                    skills = ?report.seeded,
                    "WP19 migration: backfilled the bundled skills into the company-wide layer \
                     (they were never installed because pre-v1.51.1 only the MCP create_agent \
                     path seeded them)"
                );
            }
        }
        Err(e) => {
            // A backfill failure is a nicety not delivered, never a boot
            // blocker. No marker is written, so the next boot retries.
            warn!(
                dir = %skills_dir.display(),
                error = %e,
                "WP19 migration: could not seed the bundled skills — will retry on next start"
            );
            report.error = Some(e.to_string());
            return report;
        }
    }

    write_marker(&marker, &report);
    report
}

/// Best-effort marker write. The marker doubles as the audit record: it states
/// what was written and why, in plain language, next to the timestamp.
///
/// A failure here only risks the migration running once more on the next boot,
/// which is harmless — `install_builtin_skills` skips every skill whose
/// `SKILL.md` already exists.
fn write_marker(marker: &Path, report: &SeedReport) {
    if let Some(parent) = marker.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(error = %e, "WP19 migration: could not create migrations dir — marker not written");
        return;
    }
    let record = serde_json::json!({
        "migration": "wp19-builtin-skills-seed",
        "completed_at": chrono::Utc::now().to_rfc3339(),
        "seeded_skills": report.seeded,
        "target": "<home>/skills (company-wide layer)",
        "reason": "v1.51.0 以前，內建技能（docx／xlsx／pptx／pdf 等）的種子程式只掛在 MCP \
                   create_agent 一條建立路徑上。從儀表板引導、duduclaw onboard、agent create \
                   或產業精靈建立的 AI 員工，SKILLS/ 是空的，而全公司層 <home>/skills/ 也沒有\
                   任何程式會寫入——技能庫頁「什麼都看不到」。本次回填把內建技能補到全公司層，\
                   所有現有與未來的員工都會看到。已存在的同名技能一律不覆寫。\
                   本檔存在即代表回填做過了；之後刻意刪掉的技能不會被蓋回來。",
    });
    if let Err(e) = std::fs::write(marker, format!("{record}\n")) {
        warn!(error = %e, "WP19 migration: could not write completion marker — may re-run next boot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_names(skills_dir: &Path) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(skills_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().join("SKILL.md").exists())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        names.sort();
        names
    }

    /// The whole point: an install that predates the fix has no
    /// `<home>/skills/` at all, and must come out of this with the bundled set.
    #[test]
    fn first_run_backfills_the_company_wide_layer() {
        let home = tempfile::TempDir::new().unwrap();
        assert!(!home.path().join("skills").exists(), "precondition");

        let report = run(home.path());

        assert!(!report.already_done);
        assert!(report.error.is_none());
        assert!(!report.seeded.is_empty(), "something must have been written");
        assert!(marker_path(home.path()).exists());

        let skills_root = home.path().join("skills");
        let on_disk = seeded_names(&skills_root);
        for (name, _) in duduclaw_agent::builtin_skills::BUILTIN_SKILLS {
            assert!(
                on_disk.contains(&name.to_string()),
                "bundled skill `{name}` must be backfilled, got {on_disk:?}"
            );
        }
        // The multi-file office skills (docx / xlsx / pptx / pdf) are the ones
        // the customer was actually missing — "make me a spreadsheet" is the
        // request that failed. They ship a SKILL.md *plus* the scripts it calls,
        // so a partial copy would leave a skill that lists in the dashboard and
        // then dies at runtime on a missing script.
        for (name, files) in duduclaw_agent::builtin_skills::BUILTIN_SKILL_FILES {
            assert!(
                on_disk.contains(&name.to_string()),
                "bundled office skill `{name}` must be backfilled, got {on_disk:?}"
            );
            for (rel, _) in *files {
                let mut dest = skills_root.join(name);
                for part in rel.split('/') {
                    dest.push(part);
                }
                assert!(
                    dest.is_file(),
                    "`{name}` is incomplete without `{rel}` — the skill would list \
                     in the dashboard and then fail at runtime"
                );
            }
        }
        // And the loader — the thing the dashboard actually reads through —
        // must see them. Seeding to a layout the loader ignores would leave the
        // page just as blank.
        let loaded = tokio::runtime::Runtime::new().unwrap().block_on(
            duduclaw_agent::registry::AgentRegistry::load_skills(&home.path().join("skills")),
        );
        assert!(!loaded.is_empty(), "backfilled skills must be loadable");
    }

    /// The load-bearing guarantee: after the migration has run once, an
    /// operator who deliberately deletes a bundled skill must not find it
    /// resurrected on the next start.
    #[test]
    fn marker_prevents_a_second_run() {
        let home = tempfile::TempDir::new().unwrap();
        let first = run(home.path());
        assert!(!first.seeded.is_empty());

        let victim = home.path().join("skills").join(first.seeded[0].clone());
        std::fs::remove_dir_all(&victim).unwrap();

        let second = run(home.path());
        assert!(second.already_done);
        assert!(second.seeded.is_empty());
        assert!(
            !victim.exists(),
            "a deliberately deleted skill must stay deleted"
        );
    }

    /// An operator's edited copy of a bundled skill must survive the backfill
    /// byte-for-byte.
    #[test]
    fn existing_skill_file_is_never_overwritten() {
        let home = tempfile::TempDir::new().unwrap();
        let (name, _) = duduclaw_agent::builtin_skills::BUILTIN_SKILLS[0];
        let dir = home.path().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# 我自己改過的版本\n").unwrap();

        let report = run(home.path());

        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "# 我自己改過的版本\n",
            "operator edits must win"
        );
        assert!(
            !report.seeded.contains(&name.to_string()),
            "an already-present skill must not be reported as seeded"
        );
        // The other bundled skills still get backfilled around it.
        assert!(!report.seeded.is_empty());
    }

    /// An unwritable home must degrade to a warning, not a panic and not a
    /// failed boot — and must leave no marker, so a later fix gets retried.
    #[cfg(unix)]
    #[test]
    fn seed_failure_does_not_panic_and_leaves_no_marker() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().unwrap();
        let mut perms = std::fs::metadata(home.path()).unwrap().permissions();
        perms.set_mode(0o500); // r-x: create_dir_all("skills") will fail
        std::fs::set_permissions(home.path(), perms).unwrap();

        let report = run(home.path());

        // Restore perms before assertions so TempDir cleanup works.
        let mut perms = std::fs::metadata(home.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(home.path(), perms).unwrap();

        assert!(report.seeded.is_empty());
        assert!(report.error.is_some(), "the failure must be reported");
        assert!(
            !marker_path(home.path()).exists(),
            "no marker on failure — the backfill must be retried next boot"
        );
    }

    /// Writing the marker is best-effort; if it fails the pass must still
    /// return normally (the next boot simply re-runs an idempotent seed).
    #[cfg(unix)]
    #[test]
    fn marker_write_failure_is_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().unwrap();
        // Pre-create `migrations/` read-only so the marker write fails while
        // the seed itself succeeds.
        let migrations = home.path().join("migrations");
        std::fs::create_dir_all(&migrations).unwrap();
        let mut perms = std::fs::metadata(&migrations).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&migrations, perms).unwrap();

        let report = run(home.path());

        let mut perms = std::fs::metadata(&migrations).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&migrations, perms).unwrap();

        assert!(!report.seeded.is_empty(), "the seed itself must have run");
        assert!(!marker_path(home.path()).exists());
    }
}
