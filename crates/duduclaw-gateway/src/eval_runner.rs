//! WP2.2: gateway-side driver for `duduclaw eval`.
//!
//! **Why a subprocess, not a function call** (B1, `commercial/docs/
//! DESIGN-evolution-v3-aee.md` appendix A): `duduclaw-cli` depends on
//! `duduclaw-gateway` (single-binary architecture — the CLI hosts the
//! gateway), never the other way around. The eval harness
//! (`crates/duduclaw-cli/src/eval/`) therefore cannot be called in-process
//! from here without introducing a dependency cycle. This module spawns the
//! running `duduclaw` binary itself as a child process — `duduclaw eval
//! <suite> --replay --no-judge --report <tmp>` — and parses the resulting
//! JSON report. This is the same pattern already shipped for
//! `migrate.scan`/`migrate.apply` (`handlers.rs::run_migrate_cli`): spawn
//! self, `--report`/`--json` to a machine-readable file/stdout, timeout,
//! parse.
//!
//! **Fail-closed contract**: a spawn failure, a timeout, or a missing/
//! unparseable report file is always an `Err` — this module never fabricates
//! an [`EvalReport`]. `duduclaw eval` itself exits non-zero whenever any
//! case fails (by design, so CI can gate on it); that non-zero exit is
//! *expected* here and is NOT treated as a runner failure on its own — only
//! the report file's presence/parseability decides success.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Default wall-clock budget for one `duduclaw eval --replay` subprocess.
/// Replay is offline/deterministic (no live agent calls), so this is a
/// generous ceiling against a genuinely hung process, not a tuned SLA.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// One case's outcome inside an [`EvalReport`]. Mirrors the `per_case` array
/// entries `duduclaw eval --report` writes
/// (`crates/duduclaw-cli/src/eval/mod.rs::per_case_json`). Extra fields the
/// CLI report carries (`name`, `judge_score`, `mast_class`) are intentionally
/// not part of this struct's signature (locked by WP2.2) — `serde` ignores
/// them rather than erroring, so the CLI side stays free to carry more
/// fields for its own consumers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EvalCaseResult {
    pub id: String,
    pub passed: bool,
    #[serde(default)]
    pub failed_assertions: Vec<String>,
}

/// A `duduclaw eval --report` JSON document, parsed for the fields this
/// runner's callers need. See `crates/duduclaw-cli/src/eval/mod.rs::render`
/// for the writer side of this contract.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EvalReport {
    pub suite: String,
    pub total: usize,
    pub passed: usize,
    #[serde(default)]
    pub per_case: Vec<EvalCaseResult>,
}

/// Spawns `duduclaw eval` against a suites tree and parses its `--report`
/// JSON. One instance is reusable across many `run_replay` calls.
pub struct EvalRunner {
    /// Path to the `duduclaw` binary to spawn (single binary hosts both the
    /// gateway and the `eval` CLI subcommand).
    binary: PathBuf,
    /// Root directory containing `<suite>/` case trees
    /// (e.g. `commercial/evals`).
    suites_root: PathBuf,
    /// Subprocess wall-clock budget.
    timeout: Duration,
}

impl EvalRunner {
    /// `binary` — resolve with `duduclaw_core::resolve_duduclaw_bin()`
    /// (`std::env::current_exe()` first, `DUDUCLAW_BIN` override, `"duduclaw"`
    /// on PATH as last resort — the same discovery `agent_hook_installer`
    /// already uses for spawning self). `suites_root` — the directory
    /// containing `<suite>/*.toml` trees; `run_replay`'s `suite` argument is
    /// joined onto it.
    pub fn new(binary: PathBuf, suites_root: PathBuf) -> Self {
        Self {
            binary,
            suites_root,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Override the default subprocess timeout. Mainly for tests, but also
    /// available to callers running unusually large suites.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run `<suites_root>/<suite>` in replay mode (offline, zero
    /// credentials, deterministic assertions only — `--no-judge`), optionally
    /// restricted to `case_ids` (the B4 `EvalCaseRef` filename-stem ids, via
    /// `--case`). Returns the parsed report regardless of whether individual
    /// cases passed; only spawn/timeout/report failures are `Err`.
    pub async fn run_replay(
        &self,
        suite: &str,
        case_ids: Option<&[String]>,
    ) -> anyhow::Result<EvalReport> {
        let suite_path = self.suites_root.join(suite);
        // A fresh temp *directory* holding a not-yet-created report path —
        // deliberately not `NamedTempFile` (which would create the file
        // up-front and defeat the `!report_path.exists()` "the CLI never
        // even got to writing the report" check below).
        let report_dir = tempfile::tempdir()
            .map_err(|e| anyhow::anyhow!("cannot create temp report dir: {e}"))?;
        let report_path = report_dir.path().join("eval-report.json");

        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("eval")
            .arg(&suite_path)
            .arg("--replay")
            .arg("--no-judge")
            .arg("--report")
            .arg(&report_path);
        if let Some(ids) = case_ids {
            if !ids.is_empty() {
                cmd.arg("--case").arg(ids.join(","));
            }
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "duduclaw eval timed out after {}s (suite {suite})",
                    self.timeout.as_secs()
                )
            })?
            .map_err(|e| anyhow::anyhow!("spawn duduclaw eval failed: {e}"))?;

        // `duduclaw eval` exits non-zero whenever any case fails — expected,
        // not a runner error. What IS a hard failure: the report file never
        // showing up at all (crashed before writing it, wrong args, etc).
        if !report_path.exists() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = duduclaw_core::truncate_bytes(stderr.trim(), 500);
            return Err(anyhow::anyhow!(
                "duduclaw eval produced no report (suite {suite}, exit {}): {tail}",
                output.status
            ));
        }

        let raw = std::fs::read_to_string(&report_path).map_err(|e| {
            anyhow::anyhow!("cannot read eval report {}: {e}", report_path.display())
        })?;
        let report: EvalReport = serde_json::from_str(&raw).map_err(|e| {
            anyhow::anyhow!(
                "cannot parse eval report {} as EvalReport: {e}",
                report_path.display()
            )
        })?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Build an `EvalRunner` whose "binary" is actually a tiny fake spawn
    /// target (a shell script on Unix), so tests never touch the real
    /// `duduclaw eval` harness / a live agent. Verifies the subprocess
    /// contract (argv shape, report parsing, fail-closed behavior) in
    /// isolation from the CLI crate this module cannot depend on.
    #[cfg(unix)]
    fn fake_binary(dir: &std::path::Path, script: &str) -> PathBuf {
        let path = dir.join("fake-duduclaw.sh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        write!(f, "{script}").unwrap();
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_replay_parses_report_even_when_the_run_exits_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        // A case failure makes `duduclaw eval` exit 1 — the fake mirrors
        // that: write a report, then exit non-zero.
        let script = r#"
report=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--report" ]; then
    report="$arg"
  fi
  prev="$arg"
done
cat > "$report" <<'EOF'
{"suite":"p0-ceo","total":2,"passed":1,"per_case":[
  {"id":"a","name":"a","passed":true,"failed_assertions":[],"judge_score":null,"mast_class":null},
  {"id":"b","name":"b","passed":false,"failed_assertions":["output_contains: \"x\""],"judge_score":null,"mast_class":"FM-1.1 Disobey Task Specification"}
]}
EOF
exit 1
"#;
        let bin = fake_binary(dir.path(), script);
        let runner = EvalRunner::new(bin, dir.path().to_path_buf());

        let report = runner.run_replay("p0-ceo", None).await.unwrap();
        assert_eq!(report.suite, "p0-ceo");
        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 1);
        assert_eq!(report.per_case.len(), 2);
        assert_eq!(
            report.per_case[1],
            EvalCaseResult {
                id: "b".to_string(),
                passed: false,
                failed_assertions: vec!["output_contains: \"x\"".to_string()],
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_replay_passes_suite_case_and_replay_flags() {
        let dir = tempfile::tempdir().unwrap();
        // Dump argv (minus argv[0]) into the report file itself as a JSON
        // string field, then also emit a valid report shape underneath it —
        // simplest way to assert on argv without a second spawn.
        let script = r#"
report=""
prev=""
argv=""
for arg in "$@"; do
  argv="$argv|$arg"
  if [ "$prev" = "--report" ]; then
    report="$arg"
  fi
  prev="$arg"
done
cat > "$report" <<EOF
{"suite":"$argv","total":0,"passed":0,"per_case":[]}
EOF
exit 0
"#;
        let bin = fake_binary(dir.path(), script);
        let runner = EvalRunner::new(bin, dir.path().to_path_buf());

        let ids = vec!["a".to_string(), "b".to_string()];
        let report = runner.run_replay("p0-ceo", Some(&ids)).await.unwrap();
        // `suite` field was overwritten with the captured argv dump for
        // this test only — assert the flags/values landed on argv.
        assert!(report.suite.contains("--replay"), "argv: {}", report.suite);
        assert!(report.suite.contains("--no-judge"), "argv: {}", report.suite);
        assert!(report.suite.contains("--case"), "argv: {}", report.suite);
        assert!(report.suite.contains("a,b"), "argv: {}", report.suite);
        assert!(report.suite.contains("p0-ceo"), "argv: {}", report.suite);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_report_file_is_a_hard_error_not_a_fabricated_report() {
        let dir = tempfile::tempdir().unwrap();
        // Never writes a report at all — simulates a crash before the CLI
        // gets to the `--report` write.
        let script = "echo boom 1>&2\nexit 2\n";
        let bin = fake_binary(dir.path(), script);
        let runner = EvalRunner::new(bin, dir.path().to_path_buf());

        let err = runner.run_replay("p0-ceo", None).await.unwrap_err();
        assert!(
            err.to_string().contains("produced no report"),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("boom"), "stderr tail should surface: {err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn garbage_report_json_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"
prev=""
report=""
for arg in "$@"; do
  if [ "$prev" = "--report" ]; then
    report="$arg"
  fi
  prev="$arg"
done
echo "not json" > "$report"
exit 1
"#;
        let bin = fake_binary(dir.path(), script);
        let runner = EvalRunner::new(bin, dir.path().to_path_buf());

        let err = runner.run_replay("p0-ceo", None).await.unwrap_err();
        assert!(err.to_string().contains("cannot parse eval report"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn spawn_failure_on_nonexistent_binary_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let runner = EvalRunner::new(
            dir.path().join("does-not-exist-binary"),
            dir.path().to_path_buf(),
        );
        let err = runner.run_replay("p0-ceo", None).await.unwrap_err();
        assert!(err.to_string().contains("spawn duduclaw eval failed"), "unexpected error: {err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_is_surfaced_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = "sleep 5\nexit 0\n";
        let bin = fake_binary(dir.path(), script);
        let runner =
            EvalRunner::new(bin, dir.path().to_path_buf()).with_timeout(Duration::from_millis(50));
        let err = runner.run_replay("p0-ceo", None).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "unexpected error: {err}");
    }
}
