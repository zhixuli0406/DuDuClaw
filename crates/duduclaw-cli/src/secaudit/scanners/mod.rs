//! OSS scanner detection + orchestration (DESIGN §3.2 step 2).
//!
//! For every scanner: detect via `which_cli` → not found ⇒ `EngineMissing`
//! with an honest reason (task discipline: "工具失效停工上報...絕不靜默跳過也
//! 絕不自動安裝"); found ⇒ run it (capped timeout + output, `exec.rs`) →
//! normalize its output into `Finding`s via that scanner's own module. A
//! scanner's raw stdout is untrusted DATA: a parse failure is recorded as
//! `EngineRun.parse_error`, never a panic.
//!
//! `osv-scanner` is a deliberate exception: this wave never invokes it at
//! all (see [`osv_scanner_missing`]) because it queries the network OSV
//! database, and `duduclaw secaudit` runs fully offline this wave.
//!
//! The orchestration functions here (`run_all` and friends) touch the real
//! filesystem/PATH/subprocesses and are therefore environment-dependent —
//! deliberately NOT unit-tested directly (mirrors why `duduclaw eval`'s live
//! CLI spawn path isn't unit-tested either). What IS unit-tested here are
//! the pure decision functions extracted specifically so they can be
//! (`osv_scanner_missing`, `cargo_lock_present`, `record_stdout_scanner`'s
//! outcome-classification logic). Each scanner's actual JSON normalizer is
//! fixture-tested in its own module (`gitleaks.rs`/`semgrep.rs`/`cargo_audit.rs`).

pub mod cargo_audit;
mod exec;
pub mod gitleaks;
pub mod semgrep;

use std::path::Path;

use exec::{run_capped, RunLimits};

use crate::secaudit::schema::{EngineMissing, EngineRun, Finding};

/// Run every applicable OSS scanner against `repo_root` (assumed already
/// canonicalized + validated to exist by the caller). Returns everything
/// needed to build an `AuditReport`: what ran, what didn't (+why), and every
/// normalized finding.
pub async fn run_all(repo_root: &Path) -> (Vec<EngineRun>, Vec<EngineMissing>, Vec<Finding>) {
    let mut engines_run = Vec::new();
    let mut engines_missing = Vec::new();
    let mut findings = Vec::new();

    run_semgrep(repo_root, &mut engines_run, &mut engines_missing, &mut findings).await;
    run_gitleaks(repo_root, &mut engines_run, &mut engines_missing, &mut findings).await;
    run_osv_scanner(&mut engines_missing);
    run_cargo_audit(repo_root, &mut engines_run, &mut engines_missing, &mut findings).await;

    (engines_run, engines_missing, findings)
}

/// Shared tail for scanners that stream their report as JSON on stdout
/// (semgrep, cargo-audit): classify the capped-subprocess outcome into
/// either a successful `EngineRun` (+ findings) or one with `parse_error`
/// set. Extracted as a free function (not a closure inline in each caller)
/// specifically so its branching is unit-testable without spawning a real
/// process.
fn record_stdout_scanner(
    engine: &str,
    outcome: exec::RunOutcome,
    parse: impl FnOnce(&str) -> Result<Vec<Finding>, String>,
    engines_run: &mut Vec<EngineRun>,
    findings: &mut Vec<Finding>,
) {
    if let Some(err) = outcome.spawn_error {
        engines_run.push(EngineRun {
            engine: engine.to_string(),
            findings_count: 0,
            duration_ms: outcome.duration.as_millis(),
            parse_error: Some(format!("failed to execute: {err}")),
            timed_out: false,
        });
        return;
    }
    if outcome.timed_out {
        engines_run.push(EngineRun {
            engine: engine.to_string(),
            findings_count: 0,
            duration_ms: outcome.duration.as_millis(),
            parse_error: Some(format!("timed out after {:?}", exec::DEFAULT_TIMEOUT)),
            timed_out: true,
        });
        return;
    }
    if outcome.stdout_truncated {
        // Don't attempt to parse output cut off mid-stream — truncated JSON
        // usually fails to parse anyway, but "usually" isn't good enough
        // when the failure mode would otherwise be silently-incomplete
        // findings instead of an honest, explicit report.
        engines_run.push(EngineRun {
            engine: engine.to_string(),
            findings_count: 0,
            duration_ms: outcome.duration.as_millis(),
            parse_error: Some(format!(
                "stdout truncated at the {}-byte output cap — output too large to parse reliably",
                exec::DEFAULT_MAX_OUTPUT_BYTES
            )),
            timed_out: false,
        });
        return;
    }
    match parse(&outcome.stdout) {
        Ok(parsed) => {
            let count = parsed.len();
            findings.extend(parsed);
            engines_run.push(EngineRun {
                engine: engine.to_string(),
                findings_count: count,
                duration_ms: outcome.duration.as_millis(),
                parse_error: None,
                timed_out: false,
            });
        }
        Err(e) => {
            engines_run.push(EngineRun {
                engine: engine.to_string(),
                findings_count: 0,
                duration_ms: outcome.duration.as_millis(),
                parse_error: Some(format!(
                    "{e} (exit_code={:?}, stderr_tail={:?})",
                    outcome.exit_code,
                    duduclaw_core::truncate_bytes(outcome.stderr_tail.trim(), 300)
                )),
                timed_out: false,
            });
        }
    }
}

async fn run_semgrep(
    repo_root: &Path,
    engines_run: &mut Vec<EngineRun>,
    engines_missing: &mut Vec<EngineMissing>,
    findings: &mut Vec<Finding>,
) {
    let bin = match duduclaw_core::which_cli("semgrep") {
        Some(b) => b,
        None => {
            engines_missing.push(EngineMissing {
                engine: semgrep::ENGINE_NAME.to_string(),
                reason: "not installed (not found on PATH or common install locations)".to_string(),
            });
            return;
        }
    };
    let rules_file = match semgrep::TempRulesFile::create() {
        Ok(f) => f,
        Err(e) => {
            engines_missing.push(EngineMissing {
                engine: semgrep::ENGINE_NAME.to_string(),
                reason: format!("could not stage bundled offline ruleset: {e}"),
            });
            return;
        }
    };
    let rules_path = rules_file.0.to_string_lossy().to_string();
    // Local filesystem `--config` path — never triggers a network fetch
    // (unlike `--config auto` / `--config p/<pack>`; see semgrep.rs module doc).
    // `.as_str()` (not `&rules_path`) so every array element is a plain
    // `&str`, matching the string literals with no coercion involved.
    let outcome = run_capped(
        &bin,
        &["scan", "--json", "--config", rules_path.as_str()],
        repo_root,
        &RunLimits::default(),
    )
    .await;
    record_stdout_scanner(semgrep::ENGINE_NAME, outcome, semgrep::parse, engines_run, findings);
    // `rules_file` drops here, cleaning up the temp ruleset file.
}

async fn run_gitleaks(
    repo_root: &Path,
    engines_run: &mut Vec<EngineRun>,
    engines_missing: &mut Vec<EngineMissing>,
    findings: &mut Vec<Finding>,
) {
    let bin = match duduclaw_core::which_cli("gitleaks") {
        Some(b) => b,
        None => {
            engines_missing.push(EngineMissing {
                engine: gitleaks::ENGINE_NAME.to_string(),
                reason: "not installed (not found on PATH or common install locations)".to_string(),
            });
            return;
        }
    };
    // gitleaks' stdout-vs-report-file default behavior is not verified in
    // this offline sandbox (no gitleaks binary / no network to check docs
    // against) — using the well-documented, stable `--report-path` flag to
    // a known temp location sidesteps the ambiguity entirely rather than
    // guessing at stdout parsing.
    let report_path = std::env::temp_dir().join(format!(
        "duduclaw-secaudit-gitleaks-{}.json",
        uuid::Uuid::new_v4()
    ));
    let report_path_str = report_path.to_string_lossy().to_string();
    // `.as_str()` (not `&report_path_str`) — see the semgrep call above for
    // why: keeps every array element a plain `&str`, no coercion involved.
    let outcome = run_capped(
        &bin,
        &[
            "detect",
            "--source",
            ".",
            "--report-format",
            "json",
            "--report-path",
            report_path_str.as_str(),
            "--no-banner",
            "--exit-code",
            "0",
        ],
        repo_root,
        &RunLimits::default(),
    )
    .await;

    if let Some(err) = outcome.spawn_error {
        let _ = std::fs::remove_file(&report_path);
        engines_run.push(EngineRun {
            engine: gitleaks::ENGINE_NAME.to_string(),
            findings_count: 0,
            duration_ms: outcome.duration.as_millis(),
            parse_error: Some(format!("failed to execute: {err}")),
            timed_out: false,
        });
        return;
    }
    if outcome.timed_out {
        let _ = std::fs::remove_file(&report_path);
        engines_run.push(EngineRun {
            engine: gitleaks::ENGINE_NAME.to_string(),
            findings_count: 0,
            duration_ms: outcome.duration.as_millis(),
            parse_error: Some(format!("timed out after {:?}", exec::DEFAULT_TIMEOUT)),
            timed_out: true,
        });
        return;
    }

    // Some gitleaks versions may not create the report file at all when
    // there are zero leaks — treat a clean process exit with no file as
    // "zero findings", not a failure (documented uncertainty, see above).
    // Report-file size is capped the same as stdout for the other scanners
    // (task spec: "輸出上限 16MB") — `run_capped` only bounds the piped
    // stdout/stderr it reads itself, not a file gitleaks writes on its own,
    // so this file needs its own explicit bound before we read it in full.
    let raw = match std::fs::metadata(&report_path) {
        Err(_) => Ok(String::new()),
        Ok(meta) if meta.len() > exec::DEFAULT_MAX_OUTPUT_BYTES as u64 => Err(format!(
            "gitleaks report file too large ({} bytes > {}-byte cap) — not read",
            meta.len(),
            exec::DEFAULT_MAX_OUTPUT_BYTES
        )),
        Ok(_) => {
            std::fs::read_to_string(&report_path).map_err(|e| format!("could not read gitleaks report file: {e}"))
        }
    };
    let _ = std::fs::remove_file(&report_path);

    match raw.and_then(|r| gitleaks::parse(&r)) {
        Ok(parsed) => {
            let count = parsed.len();
            findings.extend(parsed);
            engines_run.push(EngineRun {
                engine: gitleaks::ENGINE_NAME.to_string(),
                findings_count: count,
                duration_ms: outcome.duration.as_millis(),
                parse_error: None,
                timed_out: false,
            });
        }
        Err(e) => {
            engines_run.push(EngineRun {
                engine: gitleaks::ENGINE_NAME.to_string(),
                findings_count: 0,
                duration_ms: outcome.duration.as_millis(),
                parse_error: Some(format!(
                    "{e} (exit_code={:?}, stderr_tail={:?})",
                    outcome.exit_code,
                    duduclaw_core::truncate_bytes(outcome.stderr_tail.trim(), 300)
                )),
                timed_out: false,
            });
        }
    }
}

/// osv-scanner is never actually invoked this wave (§3.3 zero-network
/// discipline) — pure decision function so the reason text is unit-testable
/// without touching PATH.
fn osv_scanner_missing(installed: bool) -> EngineMissing {
    let reason = if installed {
        "installed but skipped: osv-scanner queries the network OSV vulnerability database; \
         disabled under secaudit's zero-network policy this wave — run it manually for CVE coverage"
            .to_string()
    } else {
        "not installed, and would be skipped anyway: osv-scanner requires network access to \
         query the OSV database, disabled under secaudit's zero-network policy this wave"
            .to_string()
    };
    EngineMissing {
        engine: "osv-scanner".to_string(),
        reason,
    }
}

fn run_osv_scanner(engines_missing: &mut Vec<EngineMissing>) {
    let installed = duduclaw_core::which_cli("osv-scanner").is_some();
    engines_missing.push(osv_scanner_missing(installed));
}

/// Pure filesystem check, extracted for direct unit testing.
fn cargo_lock_present(repo_root: &Path) -> bool {
    repo_root.join("Cargo.lock").exists()
}

async fn run_cargo_audit(
    repo_root: &Path,
    engines_run: &mut Vec<EngineRun>,
    engines_missing: &mut Vec<EngineMissing>,
    findings: &mut Vec<Finding>,
) {
    if !cargo_lock_present(repo_root) {
        engines_missing.push(EngineMissing {
            engine: cargo_audit::ENGINE_NAME.to_string(),
            reason: "not applicable: no Cargo.lock found at repo root".to_string(),
        });
        return;
    }
    if duduclaw_core::which_cli("cargo-audit").is_none() {
        engines_missing.push(EngineMissing {
            engine: cargo_audit::ENGINE_NAME.to_string(),
            reason: "not installed (install via `cargo install cargo-audit`)".to_string(),
        });
        return;
    }
    // Invoked via `cargo audit`, not the `cargo-audit` binary directly —
    // lets cargo's own subcommand resolution handle argv shaping.
    // `--no-fetch` uses whatever advisory-db is already cached locally
    // instead of git-fetching an update (zero-network discipline; if no
    // cache exists at all the command fails, which surfaces below as a
    // normal parse_error, not a crash).
    let outcome = run_capped(
        "cargo",
        &["audit", "--json", "--no-fetch"],
        repo_root,
        &RunLimits::default(),
    )
    .await;
    record_stdout_scanner(cargo_audit::ENGINE_NAME, outcome, cargo_audit::parse, engines_run, findings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ok_outcome(stdout: &str) -> exec::RunOutcome {
        exec::RunOutcome {
            stdout: stdout.to_string(),
            stdout_truncated: false,
            stderr_tail: String::new(),
            exit_code: Some(0),
            duration: Duration::from_millis(5),
            timed_out: false,
            spawn_error: None,
        }
    }

    #[test]
    fn record_stdout_scanner_success_path_records_findings() {
        let mut engines_run = Vec::new();
        let mut findings = Vec::new();
        record_stdout_scanner(
            "fake-engine",
            ok_outcome("anything"),
            |_raw| {
                Ok(vec![Finding::candidate(
                    "fake-engine",
                    crate::secaudit::schema::FindingKind::Other,
                    crate::secaudit::schema::Severity::Low,
                    "t",
                    "f",
                    None,
                    "s",
                    "r",
                    vec![],
                )])
            },
            &mut engines_run,
            &mut findings,
        );
        assert_eq!(engines_run.len(), 1);
        assert_eq!(engines_run[0].findings_count, 1);
        assert!(engines_run[0].parse_error.is_none());
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn record_stdout_scanner_parse_failure_sets_parse_error_not_panic() {
        let mut engines_run = Vec::new();
        let mut findings = Vec::new();
        record_stdout_scanner(
            "fake-engine",
            ok_outcome("garbage"),
            |_raw| Err("bad shape".to_string()),
            &mut engines_run,
            &mut findings,
        );
        assert_eq!(engines_run.len(), 1);
        assert_eq!(engines_run[0].findings_count, 0);
        assert!(engines_run[0].parse_error.as_ref().unwrap().contains("bad shape"));
        assert!(findings.is_empty());
    }

    #[test]
    fn record_stdout_scanner_spawn_error_is_reported() {
        let mut engines_run = Vec::new();
        let mut findings = Vec::new();
        let outcome = exec::RunOutcome {
            stdout: String::new(),
            stdout_truncated: false,
            stderr_tail: String::new(),
            exit_code: None,
            duration: Duration::from_millis(1),
            timed_out: false,
            spawn_error: Some("no such file".to_string()),
        };
        record_stdout_scanner("fake-engine", outcome, |_| Ok(vec![]), &mut engines_run, &mut findings);
        assert!(engines_run[0].parse_error.as_ref().unwrap().contains("no such file"));
    }

    #[test]
    fn record_stdout_scanner_truncated_output_is_reported_and_not_parsed() {
        let mut engines_run = Vec::new();
        let mut findings = Vec::new();
        let mut outcome = ok_outcome("truncated-garbage");
        outcome.stdout_truncated = true;
        // The parse closure must never even be called on truncated output —
        // if it were, this would panic and fail the test.
        record_stdout_scanner(
            "fake-engine",
            outcome,
            |_raw| panic!("parse must not run on truncated stdout"),
            &mut engines_run,
            &mut findings,
        );
        assert_eq!(engines_run.len(), 1);
        assert_eq!(engines_run[0].findings_count, 0);
        assert!(engines_run[0].parse_error.as_ref().unwrap().contains("truncated"));
        assert!(findings.is_empty());
    }

    #[test]
    fn record_stdout_scanner_timeout_is_reported() {
        let mut engines_run = Vec::new();
        let mut findings = Vec::new();
        let outcome = exec::RunOutcome {
            stdout: String::new(),
            stdout_truncated: false,
            stderr_tail: String::new(),
            exit_code: None,
            duration: Duration::from_secs(300),
            timed_out: true,
            spawn_error: None,
        };
        record_stdout_scanner("fake-engine", outcome, |_| Ok(vec![]), &mut engines_run, &mut findings);
        assert!(engines_run[0].timed_out);
        assert!(engines_run[0].parse_error.is_some());
    }

    #[test]
    fn osv_scanner_is_always_missing_with_a_reason_either_way() {
        let installed = osv_scanner_missing(true);
        assert_eq!(installed.engine, "osv-scanner");
        assert!(installed.reason.contains("installed but skipped"));

        let absent = osv_scanner_missing(false);
        assert!(absent.reason.contains("not installed"));
        // Both reasons must explain the *actual* cause (network policy),
        // not just "missing" — an operator who has it installed shouldn't
        // wonder why it didn't run.
        assert!(installed.reason.contains("network"));
        assert!(absent.reason.contains("network"));
    }

    #[test]
    fn cargo_lock_present_detects_file_existence() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!cargo_lock_present(dir.path()));
        std::fs::write(dir.path().join("Cargo.lock"), "").unwrap();
        assert!(cargo_lock_present(dir.path()));
    }
}
