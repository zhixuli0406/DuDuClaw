//! Versioned backfills for bundled skills: WP19 for older installs, then G1
//! for the Google setup guide. Each step keeps its own completion marker.
//!
//! **Defaults.** Only docx / xlsx / pptx / pdf are seeded by WP19.
//! Programming skills are optional. Employee creation uses this company-wide
//! backfill instead of copying bundled skills into each personal SKILLS folder.
//! Existing personal and company-wide skills are never removed.
//!
//! **Where it seeds.** `<home>/skills/`, the company-wide layer, not each
//! agent's `SKILLS/`. One write instead of N, every existing and future
//! staffer picks it up through `compose_skill_layers`, and it matches what the
//! skills actually are: shared capability, not one employee's private notes.
//!
//! **Safety.** Never overwrites operator content. A versioned marker is written
//! only after all four office bundles match the embedded script manifest.
//! Existing markers prevent re-seeding deliberate deletions, while every boot
//! still reports manifest conflicts. Scripts in skill layers never execute.
//! Failures warn without blocking boot.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Versioned manifest-verification marker. The old WP19 marker still prevents
/// re-seeding deliberate deletions, but never certifies executable content.
const MARKER_NAME: &str = "office-script-manifest-v1.done";
const LEGACY_MARKER_NAME: &str = "wp19-builtin-skills-seed.done";

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

fn safe_marker_exists(home: &Path, name: &str) -> bool {
    duduclaw_agent::skill_loader::safe_dir::SafeDir::open(home)
        .and_then(|root| root.child("migrations"))
        .and_then(|dir| dir.read_file(name)).is_ok()
}

/// Run the migration if it has not run before.
///
/// Never returns an error: a wedged migration must not stop the gateway from
/// booting. Callers get a [`SeedReport`] for logging/tests.
fn run_legacy(home_dir: &Path) -> SeedReport {
    let mut report = SeedReport::default();
    let marker = marker_path(home_dir);
    let skills_dir = home_dir.join("skills");
    let previously_seeded = safe_marker_exists(home_dir, MARKER_NAME)
        || safe_marker_exists(home_dir, LEGACY_MARKER_NAME);
    if safe_marker_exists(home_dir, MARKER_NAME) {
        report.already_done = true;
        report_office_conflicts(&skills_dir, &mut report);
        return report;
    }

    let seed = if previously_seeded { Ok(Vec::new()) } else {
        duduclaw_agent::builtin_skills::install_office_skills(&skills_dir)
    };
    match seed {
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

    if report_office_conflicts(&skills_dir, &mut report) {
        return report;
    }
    write_marker(&marker, &report);
    report
}

fn report_office_conflicts(skills_dir: &Path, report: &mut SeedReport) -> bool {
    let conflicts = duduclaw_agent::builtin_skills::office_script_conflicts(skills_dir);
    if conflicts.is_empty() { return false; }
    for conflict in &conflicts {
        warn!(%conflict, "office script conflict: company copy ignored; execution uses embedded scripts only");
    }
    report.error = Some(format!("office script conflicts: {}", conflicts.join("; ")));
    true
}

/// Run both independently versioned backfills; an old WP19 marker must not
/// suppress newly bundled skills. Neither pass overwrites operator content.
pub fn run(home_dir: &Path) -> SeedReport {
    let google = run_google_setup(home_dir);
    // On supported platforms, reject unsafe skills paths before the legacy installer runs.
    // Unsupported secure creation elsewhere must not suppress legacy skills.
    if cfg!(any(unix, windows)) && google.error.is_some() {
        return google;
    }
    let mut report = run_legacy(home_dir);
    report.seeded.extend(google.seeded);
    report.already_done = report.already_done && google.already_done;
    if report.error.is_none() {
        report.error = google.error;
    }
    report
}

const GOOGLE_MARKER_NAME: &str = "g1-connect-google-seed-v1.done";

fn run_google_setup(home_dir: &Path) -> SeedReport {
    let mut report = SeedReport::default();
    let marker = home_dir.join("migrations").join(GOOGLE_MARKER_NAME);
    if marker.exists() {
        report.already_done = true;
        return report;
    }
    let result = (|| -> std::io::Result<()> {
        let (_, body) = duduclaw_agent::builtin_skills::BUILTIN_SKILLS.iter()
            .find(|(name, _)| *name == "connect-google")
            .expect("connect-google must be bundled");
        let root = crate::fs_safe::SafeDir::open_root(home_dir)?;
        let dir = root.open_path(&["skills", "connect-google"], true)?
            .ok_or_else(|| crate::fs_safe::refused("missing skills directory"))?;
        if dir.create_file_new("SKILL.md", body.as_bytes())? {
            report.seeded.push("connect-google".to_string());
        }
        std::fs::create_dir_all(home_dir.join("migrations"))?;
        std::fs::write(&marker, "g1-connect-google-seed-v1\n")?;
        Ok(())
    })();
    if let Err(e) = result {
        warn!(error = %e, "Google setup skill backfill failed; will retry next boot");
        report.error = Some(e.to_string());
    }
    report
}

/// Best-effort marker write. The marker doubles as the audit record: it states
/// what was written and why, in plain language, next to the timestamp.
///
/// A failure here only risks the migration running once more on the next boot,
/// which is harmless — `install_office_skills` skips every skill whose
/// `SKILL.md` already exists.
fn write_marker(marker: &Path, report: &SeedReport) {
    let record = serde_json::json!({
        "migration": "office-script-manifest-v1",
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
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let home = marker.parent().and_then(Path::parent)
            .ok_or_else(|| std::io::Error::other("missing migration parent"))?;
        let root = duduclaw_agent::skill_loader::safe_dir::SafeDir::open(home)?;
        let migrations = root.mkdir_child("migrations", 0o700)?;
        let mut file = migrations.create_new(MARKER_NAME, 0o600)?;
        file.write_all(format!("{record}\n").as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = result {
        warn!(error = %e, "WP19 migration: could not write completion marker — may re-run next boot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn google_backfill_runs_after_legacy_marker_and_preserves_operator_changes() {
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        std::fs::create_dir_all(home.path().join("migrations")).unwrap();
        std::fs::write(marker_path(home.path()), "legacy").unwrap();
        assert_eq!(run(home.path()).seeded, ["connect-google"]);
        assert_eq!(seeded_names(&home.path().join("skills")), ["connect-google"]);
        let file = home.path().join("skills/connect-google/SKILL.md");
        std::fs::remove_file(&file).unwrap();
        assert!(run(home.path()).already_done);
        assert!(!file.exists(), "deliberate deletion must survive restart");

        let other = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let dir = other.path().join("skills/connect-google");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "operator edition").unwrap();
        assert!(run_google_setup(other.path()).seeded.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), "operator edition");
    }

    #[cfg(windows)]
    #[test]
    fn google_backfill_refuses_junction_before_legacy_seeding() {
        for component in ["skills", "skills/connect-google"] {
            let home = crate::fs_safe::private_test_home().unwrap();
            let outside = crate::fs_safe::private_test_home().unwrap();
            let junction = home.path().join(component);
            std::fs::create_dir_all(junction.parent().unwrap()).unwrap();
            let output = std::process::Command::new("cmd").args(["/d", "/c", "mklink", "/J"])
                .arg(&junction).arg(outside.path()).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let report = run(home.path());
            assert!(report.error.is_some());
            assert!(report.seeded.is_empty());
            assert!(!home.path().join("migrations").exists());
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
            std::fs::remove_dir(&junction).unwrap();
        }
    }

    // Not run: reject both directory components and the final file.
    #[cfg(unix)]
    #[test]
    fn google_backfill_refuses_symlinks_without_writing_outside() {
        use std::os::unix::fs::symlink;
        for component in ["skills", "connect-google", "SKILL.md"] {
            let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let outside = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let target = match component {
                "skills" => home.path().join("skills"),
                "connect-google" => {
                    std::fs::create_dir(home.path().join("skills")).unwrap();
                    home.path().join("skills/connect-google")
                }
                _ => {
                    std::fs::create_dir_all(home.path().join("skills/connect-google")).unwrap();
                    home.path().join("skills/connect-google/SKILL.md")
                }
            };
            let destination = if component == "SKILL.md" { outside.path().join("missing-file") } else { outside.path().to_path_buf() };
            symlink(&destination, &target).unwrap();
            let report = run_google_setup(home.path());
            assert!(report.error.is_some(), "{component}");
            assert!(report.seeded.is_empty());
            assert!(!home.path().join("migrations").join(GOOGLE_MARKER_NAME).exists());
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_backfill_cannot_bypass_google_marker_with_a_symlink() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let outside = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        std::fs::create_dir_all(home.path().join("migrations")).unwrap();
        std::fs::write(home.path().join("migrations").join(GOOGLE_MARKER_NAME), "done").unwrap();
        std::fs::create_dir(home.path().join("skills")).unwrap();
        symlink(outside.path(), home.path().join("skills/connect-google")).unwrap();
        let report = run(home.path());
        assert!(!report.seeded.contains(&"connect-google".to_string()));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn office_conflict_never_gets_completion_marker() {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let home = temp.path().canonicalize().unwrap();
        duduclaw_agent::builtin_skills::install_office_skills(&home.join("skills")).unwrap();
        let script = home.join("skills/pdf/scripts/create.py");
        std::fs::write(&script, "print('attacker')").unwrap();
        let report = run(&home);
        assert!(report.error.as_deref().unwrap().contains("office script conflicts"));
        assert!(!marker_path(&home).exists());
        assert_eq!(std::fs::read_to_string(&script).unwrap(), "print('attacker')");
        assert!(run(&home).error.is_some(), "conflicts must remain observable on retry");
    }

    #[cfg(unix)]
    #[test]
    fn legacy_marker_does_not_certify_modified_scripts() {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let home = temp.path().canonicalize().unwrap();
        std::fs::create_dir(home.join("migrations")).unwrap();
        std::fs::write(home.join("migrations").join(LEGACY_MARKER_NAME), "legacy").unwrap();
        assert!(run(&home).error.is_some());
        assert!(!marker_path(&home).exists());
        assert!(!home.join("skills/pdf").exists(), "old marker still prevents restoring deleted skills");
    }

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
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        assert!(!home.path().join("skills").exists(), "precondition");

        let report = run(home.path());

        assert!(!report.already_done);
        #[cfg(any(unix, windows))]
        assert!(report.error.is_none());
        #[cfg(not(any(unix, windows)))]
        assert!(report.error.is_some(), "Google backfill must fail closed without secure creation");
        assert!(
            !report.seeded.is_empty(),
            "something must have been written"
        );
        assert!(marker_path(home.path()).exists());

        let skills_root = home.path().join("skills");
        let on_disk = seeded_names(&skills_root);
        for dev in ["code-review", "refactor", "test-writer", "git-workflow"] {
            assert!(!on_disk.contains(&dev.to_string()));
        }
        if cfg!(any(unix, windows)) { assert!(on_disk.contains(&"connect-google".to_string())); }
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
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let first = run(home.path());
        assert!(!first.seeded.is_empty());

        let victim = home.path().join("skills").join(first.seeded[0].clone());
        std::fs::remove_dir_all(&victim).unwrap();

        let second = run(home.path());
        assert_eq!(second.already_done, cfg!(any(unix, windows)));
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
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
        let home = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
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
