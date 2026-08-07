//! WP2.2 / appendix A — the concrete [`MeasureScorer`] behind the AEE seam.
//!
//! [`crate::gvu::verifier_measure::NullScorer`] was always a placeholder: it
//! makes the `cases` dimension *absent*, which is honest but leaves the commit
//! gate with nothing but the three deterministic content dimensions. This
//! module is the adapter that finally gives AEE a real one, by wrapping
//! [`crate::eval_runner::EvalRunner`] — which spawns the running `duduclaw`
//! binary as `duduclaw eval <suite> --replay --no-judge --report <tmp>` and
//! parses the JSON report (`duduclaw-cli` depends on `duduclaw-gateway`, never
//! the reverse, so a subprocess is the only in-tree way across that edge).
//!
//! ## Two id conventions, reconciled here
//!
//! The two halves of this feature landed with different notions of a case's
//! identity, and the seam is exactly here:
//!
//! | Side | Identity | Where |
//! |---|---|---|
//! | gateway (`EvalCaseRef`) | `<suite>/<[case] name>` | `playbook::delta::eval_case_exists` |
//! | CLI (`--case`) | the file's **filename stem** | `duduclaw-cli::eval::case_id` |
//!
//! Across the 360 shipped cases in `commercial/evals/` the two coincide
//! (`name == stem` for every single one, verified), but relying on that would
//! be a latent mis-selection the moment somebody writes a case where they
//! differ. So this module indexes the suite once per scoring call and
//! translates in both directions: ref → stem going in (`--case`), stem → ref
//! coming out (so `CaseScore::case` is a string the playbook's `eval_cases`
//! can actually be matched against).
//!
//! ## Degradation contract
//!
//! Never fabricates a score, and never lets an infrastructure problem look
//! like a quality regression:
//!
//! | Situation | Result | Downstream meaning |
//! |---|---|---|
//! | suites root or suite directory absent | `Ok(None)` | dimension absent (identical to `NullScorer`) |
//! | no requested ref resolves to a real case | `Ok(None)` | dimension absent |
//! | subprocess spawn / timeout / unparseable report | `Err(_)` | dimension absent **and** `scorer_error` surfaced + `warn!` |
//!
//! Evolution is never blocked by any of these — Measure simply compares the
//! dimensions that exist (`verifier_measure::commit_verdict` skips a dimension
//! present on only one side).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tracing::{debug, warn};

use crate::eval_runner::EvalRunner;
use crate::gvu::verifier_measure::{CaseScore, MeasureScorer, ScoreRequest};

use super::prompt::HOLDOUT_SEGMENTS;

/// Environment override for the eval suites root — highest precedence, so an
/// operator (or a test) can point AEE at a tree without editing config.
pub const EVAL_SUITES_ROOT_ENV: &str = "DUDUCLAW_EVAL_SUITES_ROOT";

/// Directory name of the default (runtime) suites root under the DuDuClaw
/// home: `~/.duduclaw/evals`.
///
/// Deliberately **not** the repo's `commercial/evals` — that path exists only
/// on a developer checkout, and baking it into the binary would ship a
/// dangling path to every installed copy. Deployments install the corpus to
/// the runtime root; a developer points `[evolution] eval_suites_root` (or
/// [`EVAL_SUITES_ROOT_ENV`]) at the checkout.
pub const DEFAULT_EVAL_SUITES_DIRNAME: &str = "evals";

/// Resolve the eval suites root for this installation.
///
/// Precedence: [`EVAL_SUITES_ROOT_ENV`] → `config.toml [evolution]
/// eval_suites_root` (relative values resolve against `home_dir`) →
/// `<home_dir>/evals`. Malformed config degrades to the default rather than
/// failing the round.
pub fn resolve_eval_suites_root(home_dir: &Path) -> PathBuf {
    if let Ok(v) = std::env::var(EVAL_SUITES_ROOT_ENV) {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(configured) = config_suites_root(home_dir) {
        return if configured.is_absolute() {
            configured
        } else {
            home_dir.join(configured)
        };
    }
    home_dir.join(DEFAULT_EVAL_SUITES_DIRNAME)
}

/// Resolve the binary the eval subprocess is spawned from.
///
/// `config.toml [evolution] eval_binary` wins when set; otherwise
/// [`duduclaw_core::resolve_duduclaw_bin`] (`DUDUCLAW_BIN` → `current_exe` →
/// `duduclaw` on PATH). The config key exists because `current_exe` is the
/// right answer only when the gateway *is* the shipped `duduclaw` binary —
/// under a wrapper, a symlink farm, or a test harness it is not, and an
/// operator needs a way to say so that does not involve the process
/// environment.
pub fn resolve_eval_binary(home_dir: &Path) -> PathBuf {
    match config_string(home_dir, "eval_binary") {
        Some(s) => PathBuf::from(s),
        None => duduclaw_core::resolve_duduclaw_bin(),
    }
}

fn config_suites_root(home_dir: &Path) -> Option<PathBuf> {
    config_string(home_dir, "eval_suites_root").map(PathBuf::from)
}

fn config_string(home_dir: &Path, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    let value = raw.parse::<toml::Value>().ok()?;
    let s = value
        .get("evolution")
        .and_then(|e| e.get(key))
        .and_then(|v| v.as_str())?
        .trim();
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

/// One case file as it exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedCase {
    /// Filename stem — what `duduclaw eval --case` matches on.
    stem: String,
    /// `[case] name` — what an `EvalCaseRef`'s second half is.
    name: String,
    /// Lives under a held-out path segment.
    held_out: bool,
    /// A recorded transcript exists, so `--replay` can actually evaluate it.
    ///
    /// This is load-bearing, not bookkeeping: `duduclaw eval --replay` reports
    /// a case with no transcript as **failed** ("replay transcript missing …
    /// run once with --record"). Passing that through as `0.0` would turn a
    /// missing recording — an infrastructure gap — into a quality score, and a
    /// whole unrecorded corpus into a champion of straight zeroes that nothing
    /// can ever improve on. Unrecorded cases are therefore dropped from the
    /// result, which is precisely the [`MeasureScorer`] contract's `Ok(None)`
    /// case ("no suite, no transcript, scorer intentionally inert").
    has_transcript: bool,
}

/// Every `*.toml` case under `<root>/<suite>`, recursively.
///
/// Recursive because held-out cases live in a subdirectory (`held-out/`) and
/// the CLI's own discovery is recursive — an index that stopped at the top
/// level would silently classify every held-out case as "unknown" and drop it
/// from the outer measurement, which is precisely the comparison that catches
/// over-fitting.
fn index_suite(root: &Path, suite: &str) -> Vec<IndexedCase> {
    let mut out = Vec::new();
    collect_cases(&root.join(suite), 0, &mut out);
    out.sort_by(|a, b| a.stem.cmp(&b.stem));
    out.dedup_by(|a, b| a.stem == b.stem);
    out
}

/// Depth bound: the shipped corpus is `<suite>/[held-out/]<case>.toml`, so 4
/// is generous. A bound (rather than unbounded recursion) keeps a symlink
/// loop from turning a scoring call into a hang.
const MAX_SUITE_DEPTH: usize = 4;

fn collect_cases(dir: &Path, depth: usize, out: &mut Vec<IndexedCase>) {
    if depth > MAX_SUITE_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_cases(&path, depth + 1, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.parse::<toml::Value>().ok());
        let case_table = parsed.as_ref().and_then(|v| v.get("case"));
        let name = case_table
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| stem.to_string());
        // Mirrors `duduclaw-cli::eval::runner::transcript_path`: an explicit
        // `[case] transcript` relative to the case file, else
        // `<stem>.transcript.jsonl` beside it.
        let transcript = match case_table.and_then(|c| c.get("transcript")).and_then(|t| t.as_str())
        {
            Some(rel) => dir.join(rel),
            None => dir.join(format!("{stem}.transcript.jsonl")),
        };
        out.push(IndexedCase {
            stem: stem.to_string(),
            name,
            held_out: path_is_held_out(&path),
            has_transcript: transcript.is_file(),
        });
    }
}

/// Segment-exact held-out detection over a filesystem path — the same
/// convention [`super::prompt::is_holdout`] applies to an `EvalCaseRef`, so a
/// case cannot be held-out on one side of the seam and ordinary on the other.
fn path_is_held_out(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| HOLDOUT_SEGMENTS.contains(&s))
    })
}

/// The eval-backed [`MeasureScorer`].
///
/// The suite for an agent is the directory named after the agent
/// (`<suites_root>/<agent_id>`), matching the shipped
/// `commercial/evals/<agent-id>/` layout.
pub struct EvalMeasureScorer {
    runner: EvalRunner,
    suites_root: PathBuf,
}

impl EvalMeasureScorer {
    /// Wrap an explicit binary + suites root. Mainly for tests, which point
    /// `binary` at a fake `duduclaw` script.
    pub fn with_binary(binary: PathBuf, suites_root: PathBuf) -> Self {
        Self {
            runner: EvalRunner::new(binary, suites_root.clone()),
            suites_root,
        }
    }

    /// The production constructor: spawn *this* binary (`current_exe`, with
    /// the `DUDUCLAW_BIN` override and a `PATH` fallback — the same discovery
    /// `agent_hook_installer` uses), against the resolved suites root.
    pub fn from_home(home_dir: &Path) -> Self {
        Self::with_binary(
            resolve_eval_binary(home_dir),
            resolve_eval_suites_root(home_dir),
        )
    }

    pub fn suites_root(&self) -> &Path {
        &self.suites_root
    }

    /// Does this agent have a suite at all? A `false` here is the ordinary
    /// "this agent has no eval corpus" state, not a failure.
    pub fn has_suite(&self, agent_id: &str) -> bool {
        self.suites_root.join(agent_id).is_dir()
    }
}

#[async_trait]
impl MeasureScorer for EvalMeasureScorer {
    async fn score(&self, req: &ScoreRequest) -> Result<Option<Vec<CaseScore>>, String> {
        let suite = req.agent_id.as_str();
        if suite.is_empty() || !self.has_suite(suite) {
            debug!(
                agent = %req.agent_id,
                root = %self.suites_root.display(),
                "AEE eval scorer: no suite for this agent — case dimension absent"
            );
            return Ok(None);
        }
        let index = index_suite(&self.suites_root, suite);
        if index.is_empty() {
            debug!(agent = %req.agent_id, "AEE eval scorer: suite directory has no cases");
            return Ok(None);
        }
        if !index.iter().any(|c| c.has_transcript) {
            // The whole corpus is unrecorded. Replay would report every case
            // as failed for want of a transcript; reporting that as a wall of
            // 0.0 would be a fabricated quality signal.
            warn!(
                agent = %req.agent_id,
                suite,
                cases = index.len(),
                "AEE eval scorer: suite has no recorded transcripts — case dimension ABSENT. \
                 Run `duduclaw eval <suite> --record` once to make replay meaningful."
            );
            return Ok(None);
        }

        // ── Which case ids does the CLI need to run? ─────────────────────
        //
        // Always an explicit id list (never "let the CLI discover the whole
        // tree"), because unrecorded cases must be excluded from the run, and
        // held-out cases must be excluded unless asked for.
        let selected: Option<Vec<String>> = if req.cases.is_empty() {
            let ids: Vec<String> = index
                .iter()
                .filter(|c| c.has_transcript)
                .filter(|c| req.include_holdout || !c.held_out)
                .map(|c| c.stem.clone())
                .collect();
            if ids.is_empty() {
                return Ok(None);
            }
            Some(ids)
        } else {
            let mut ids = Vec::new();
            let mut unknown = Vec::new();
            for r in &req.cases {
                let wanted = r.rsplit_once('/').map(|(_, c)| c).unwrap_or(r.as_str());
                match index
                    .iter()
                    .find(|c| c.name == wanted || c.stem == wanted)
                    .filter(|c| req.include_holdout || !c.held_out)
                    .filter(|c| c.has_transcript)
                {
                    Some(c) => ids.push(c.stem.clone()),
                    None => unknown.push(r.clone()),
                }
            }
            if !unknown.is_empty() {
                // Reported, never silently substituted: a ref that does not
                // resolve is a broken link in the playbook, and pretending it
                // scored would be a fabricated result.
                debug!(
                    agent = %req.agent_id,
                    unresolved = ?unknown,
                    "AEE eval scorer: some requested case refs do not exist in the suite"
                );
            }
            if ids.is_empty() {
                return Ok(None);
            }
            ids.sort();
            ids.dedup();
            Some(ids)
        };

        let report = self
            .runner
            .run_replay(suite, selected.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        let mut scores = Vec::with_capacity(report.per_case.len());
        for case in &report.per_case {
            let Some(indexed) = index.iter().find(|c| c.stem == case.id) else {
                // The report named a case the index does not know — possible
                // if the tree changed under us mid-run. Skipping is honest;
                // inventing an `EvalCaseRef` for it would poison the
                // entry-level settlement that matches on that string.
                warn!(
                    agent = %req.agent_id,
                    case = %case.id,
                    "AEE eval scorer: report case is not in the suite index — dropped"
                );
                continue;
            };
            if indexed.held_out && !req.include_holdout {
                continue;
            }
            if !indexed.has_transcript {
                // Belt and braces: a case the CLI ran anyway (an explicit
                // `[case] transcript` that vanished between index and run) is
                // unmeasured, not failed.
                continue;
            }
            scores.push(CaseScore {
                case: format!("{suite}/{}", indexed.name),
                score: if case.passed { 1.0 } else { 0.0 },
                held_out: indexed.held_out,
            });
        }
        Ok(Some(scores))
    }

    fn name(&self) -> &'static str {
        "eval-replay"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Write a suite. Every case gets a (dummy) recorded transcript beside it,
    /// because an unrecorded case is deliberately unmeasurable — see
    /// [`IndexedCase::has_transcript`] and
    /// `unrecorded_corpus_is_absent_not_a_wall_of_zeroes`.
    fn suite_with(root: &Path, suite: &str, cases: &[(&str, &str)], holdout: &[(&str, &str)]) {
        write_suite(root, suite, cases, holdout, true)
    }

    fn write_suite(
        root: &Path,
        suite: &str,
        cases: &[(&str, &str)],
        holdout: &[(&str, &str)],
        record: bool,
    ) {
        let dir = root.join(suite);
        std::fs::create_dir_all(&dir).unwrap();
        let mut write_case = |dir: &Path, stem: &str, name: &str| {
            std::fs::write(
                dir.join(format!("{stem}.toml")),
                format!("[case]\nname = \"{name}\"\nagent = \"a\"\nprompt = \"hi\"\n"),
            )
            .unwrap();
            if record {
                std::fs::write(dir.join(format!("{stem}.transcript.jsonl")), "{}\n").unwrap();
            }
        };
        for (stem, name) in cases {
            write_case(&dir, stem, name);
        }
        if !holdout.is_empty() {
            let hd = dir.join("held-out");
            std::fs::create_dir_all(&hd).unwrap();
            for (stem, name) in holdout {
                write_case(&hd, stem, name);
            }
        }
    }

    /// A fake `duduclaw` that echoes the argv it saw into the report's
    /// `suite` field and passes/fails cases by a naming convention.
    #[cfg(unix)]
    fn fake_binary(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("fake-duduclaw.sh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        write!(f, "{script}").unwrap();
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn suites_root_precedence_env_then_config_then_default() {
        let home = tempfile::tempdir().unwrap();
        // Default.
        assert_eq!(
            resolve_eval_suites_root(home.path()),
            home.path().join("evals")
        );
        // config.toml, relative → resolved against home.
        std::fs::write(
            home.path().join("config.toml"),
            "[evolution]\neval_suites_root = \"corpus/evals\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_eval_suites_root(home.path()),
            home.path().join("corpus/evals")
        );
        // config.toml, absolute → used verbatim.
        std::fs::write(
            home.path().join("config.toml"),
            "[evolution]\neval_suites_root = \"/opt/evals\"\n",
        )
        .unwrap();
        assert_eq!(resolve_eval_suites_root(home.path()), PathBuf::from("/opt/evals"));
        // Malformed TOML degrades to the default, never panics.
        std::fs::write(home.path().join("config.toml"), "not = = toml").unwrap();
        assert_eq!(
            resolve_eval_suites_root(home.path()),
            home.path().join("evals")
        );
    }

    #[tokio::test]
    async fn unrecorded_corpus_is_absent_not_a_wall_of_zeroes() {
        // The state the shipped `commercial/evals/` corpus is actually in
        // today: 360 cases, zero recorded transcripts. `duduclaw eval
        // --replay` reports every one of them as FAILED for want of a
        // transcript. If that were passed through as 0.0, the champion
        // bootstrap would enshrine a vector of zeroes and every later
        // candidate would tie against it forever.
        let root = tempfile::tempdir().unwrap();
        write_suite(root.path(), "a0", &[("c1", "c1"), ("c2", "c2")], &[], false);
        let scorer = EvalMeasureScorer::with_binary(
            root.path().join("never-spawned"),
            root.path().to_path_buf(),
        );
        let out = scorer
            .score(&ScoreRequest { agent_id: "a0".into(), ..Default::default() })
            .await
            .unwrap();
        assert!(out.is_none(), "an unrecorded corpus must read as ABSENT, never as failing");
    }

    #[test]
    fn index_reads_case_name_and_flags_held_out_by_segment() {
        let root = tempfile::tempdir().unwrap();
        suite_with(
            root.path(),
            "a1",
            &[("refund", "refund-flow"), ("escalate", "escalate")],
            &[("secret", "secret-probe")],
        );
        let index = index_suite(root.path(), "a1");
        assert_eq!(index.len(), 3);
        let refund = index.iter().find(|c| c.stem == "refund").unwrap();
        assert_eq!(refund.name, "refund-flow", "[case] name is the ref half");
        assert!(!refund.held_out);
        assert!(index.iter().find(|c| c.stem == "secret").unwrap().held_out);
    }

    #[tokio::test]
    async fn missing_suite_is_absent_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let scorer = EvalMeasureScorer::with_binary(
            root.path().join("never-spawned"),
            root.path().to_path_buf(),
        );
        let out = scorer
            .score(&ScoreRequest { agent_id: "ghost".into(), ..Default::default() })
            .await
            .unwrap();
        assert!(out.is_none(), "no suite ⇒ absent dimension, identical to NullScorer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn whole_suite_without_holdout_selects_every_visible_case_by_id() {
        let root = tempfile::tempdir().unwrap();
        suite_with(
            root.path(),
            "a2",
            &[("c1", "c1"), ("c2", "c2")],
            &[("h1", "h1")],
        );
        // Report every id it was asked for as a pass, and echo argv.
        let script = r#"
report=""
prev=""
argv=""
for arg in "$@"; do
  argv="$argv $arg"
  if [ "$prev" = "--report" ]; then report="$arg"; fi
  prev="$arg"
done
cat > "$report" <<EOF
{"suite":"a2","total":2,"passed":2,"per_case":[
 {"id":"c1","passed":true,"failed_assertions":[]},
 {"id":"c2","passed":false,"failed_assertions":["x"]}
],"argv":"$argv"}
EOF
exit 0
"#;
        let bin = fake_binary(root.path(), script);
        let scorer = EvalMeasureScorer::with_binary(bin, root.path().to_path_buf());
        let scores = scorer
            .score(&ScoreRequest {
                agent_id: "a2".into(),
                cases: Vec::new(),
                include_holdout: false,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].case, "a2/c1");
        assert_eq!(scores[0].score, 1.0);
        assert_eq!(scores[1].score, 0.0, "a failed case is 0.0, not absent");
        assert!(scores.iter().all(|s| !s.held_out));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn held_out_case_is_dropped_unless_explicitly_requested() {
        let root = tempfile::tempdir().unwrap();
        suite_with(root.path(), "a3", &[("c1", "c1")], &[("h1", "h1")]);
        let script = r#"
report=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--report" ]; then report="$arg"; fi
  prev="$arg"
done
cat > "$report" <<'EOF'
{"suite":"a3","total":2,"passed":2,"per_case":[
 {"id":"c1","passed":true,"failed_assertions":[]},
 {"id":"h1","passed":true,"failed_assertions":[]}
]}
EOF
exit 0
"#;
        let bin = fake_binary(root.path(), script);
        let scorer = EvalMeasureScorer::with_binary(bin, root.path().to_path_buf());

        let visible = scorer
            .score(&ScoreRequest { agent_id: "a3".into(), cases: Vec::new(), include_holdout: false })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.len(), 1, "held-out never reaches the inner loop");

        let full = scorer
            .score(&ScoreRequest { agent_id: "a3".into(), cases: Vec::new(), include_holdout: true })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(full.len(), 2);
        assert!(full.iter().any(|s| s.held_out && s.case == "a3/h1"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn requested_refs_translate_name_to_filename_stem() {
        let root = tempfile::tempdir().unwrap();
        // The interesting case: stem != [case] name.
        suite_with(root.path(), "a4", &[("case-01", "refund-flow")], &[]);
        let script = r#"
report=""
prev=""
argv=""
for arg in "$@"; do
  argv="$argv$arg,"
  if [ "$prev" = "--report" ]; then report="$arg"; fi
  prev="$arg"
done
case "$argv" in
  *case-01*) cat > "$report" <<'EOF'
{"suite":"a4","total":1,"passed":1,"per_case":[{"id":"case-01","passed":true,"failed_assertions":[]}]}
EOF
  ;;
  *) cat > "$report" <<'EOF'
{"suite":"a4","total":0,"passed":0,"per_case":[]}
EOF
  ;;
esac
exit 0
"#;
        let bin = fake_binary(root.path(), script);
        let scorer = EvalMeasureScorer::with_binary(bin, root.path().to_path_buf());
        let scores = scorer
            .score(&ScoreRequest {
                agent_id: "a4".into(),
                cases: vec!["a4/refund-flow".to_string()],
                include_holdout: false,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scores.len(), 1, "the ref's `name` half resolved to the file stem");
        assert_eq!(scores[0].case, "a4/refund-flow", "and translated back on the way out");
    }

    #[tokio::test]
    async fn unresolvable_refs_yield_absent_not_a_zero() {
        let root = tempfile::tempdir().unwrap();
        suite_with(root.path(), "a5", &[("c1", "c1")], &[]);
        let scorer = EvalMeasureScorer::with_binary(
            root.path().join("never-spawned"),
            root.path().to_path_buf(),
        );
        let out = scorer
            .score(&ScoreRequest {
                agent_id: "a5".into(),
                cases: vec!["a5/does-not-exist".into()],
                include_holdout: false,
            })
            .await
            .unwrap();
        assert!(out.is_none(), "unknown refs must not become 0.0 scores");
    }

    #[tokio::test]
    async fn spawn_failure_is_an_error_so_the_caller_can_report_it() {
        let root = tempfile::tempdir().unwrap();
        suite_with(root.path(), "a6", &[("c1", "c1")], &[]);
        let scorer = EvalMeasureScorer::with_binary(
            root.path().join("no-such-binary"),
            root.path().to_path_buf(),
        );
        let err = scorer
            .score(&ScoreRequest { agent_id: "a6".into(), cases: Vec::new(), include_holdout: false })
            .await
            .unwrap_err();
        assert!(err.contains("spawn duduclaw eval failed"), "unexpected: {err}");
    }
}
