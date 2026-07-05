//! Eval case format — `evals/<suite>/<case>.toml` loader + validation.
//!
//! One TOML file per case (ADK-evalset style): a `[case]` block naming the
//! agent + prompt, an `[expect]` block of deterministic assertions over the
//! parsed stream-json transcript, and an optional `[judge]` rubric scored by
//! an LLM judge (Braintrust scorer style). Validation fails fast at load time
//! so a typo'd suite never half-runs.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default model when a case doesn't pin one. Matches the platform default
/// used across `agent.toml [model] preferred`.
pub const DEFAULT_EVAL_MODEL: &str = "claude-sonnet-4-6";

const DEFAULT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_MAX_TURNS: u32 = 25;
const DEFAULT_MIN_SCORE: f64 = 0.7;

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
fn default_max_turns() -> u32 {
    DEFAULT_MAX_TURNS
}
fn default_min_score() -> f64 {
    DEFAULT_MIN_SCORE
}
fn default_true() -> bool {
    true
}

/// A whole `<case>.toml` file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCaseFile {
    pub case: CaseMeta,
    #[serde(default)]
    pub expect: ExpectSpec,
    pub judge: Option<JudgeSpec>,
}

/// `[case]` — who to run and what to say.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseMeta {
    /// Case name shown in reports. `[a-zA-Z0-9_-]`, ≤64 chars.
    pub name: String,
    /// Agent id under `<home>/agents/<agent>` (live mode only; replay mode
    /// never touches the agent directory).
    pub agent: String,
    /// The user prompt sent to the agent.
    pub prompt: String,
    /// Optional system prompt override passed via `--system-prompt-file`.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Model id (default [`DEFAULT_EVAL_MODEL`]).
    #[serde(default)]
    pub model: Option<String>,
    /// Live-run wall-clock budget.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// `--max-turns` passed to the CLI.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    /// Replay transcript file, relative to the case file's directory.
    /// Defaults to `<case-file-stem>.transcript.jsonl`.
    #[serde(default)]
    pub transcript: Option<String>,
}

/// `[expect]` — deterministic, zero-LLM assertions over the transcript.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectSpec {
    /// Every listed tool must appear at least once among `tool_use` blocks.
    /// Matches the exact tool name or the final `__`-delimited segment
    /// (so `tasks_create` matches `mcp__duduclaw__tasks_create`).
    #[serde(default)]
    pub must_use_tools: Vec<String>,
    /// None of the listed tools may appear (same matching rule).
    #[serde(default)]
    pub must_not_use_tools: Vec<String>,
    /// Each string must appear in the final answer text (case-sensitive).
    #[serde(default)]
    pub output_contains: Vec<String>,
    /// None of these strings may appear in the final answer text.
    #[serde(default)]
    pub output_not_contains: Vec<String>,
    /// Regex the final answer text must match.
    #[serde(default)]
    pub output_regex: Option<String>,
    /// Minimum number of assistant text blocks in the transcript.
    #[serde(default)]
    pub min_text_blocks: Option<u32>,
    /// Upper bound on total `tool_use` blocks (budget guard).
    #[serde(default)]
    pub max_tool_calls: Option<u32>,
}

impl ExpectSpec {
    /// True when no deterministic assertion is configured.
    pub fn is_empty(&self) -> bool {
        self.must_use_tools.is_empty()
            && self.must_not_use_tools.is_empty()
            && self.output_contains.is_empty()
            && self.output_not_contains.is_empty()
            && self.output_regex.is_none()
            && self.min_text_blocks.is_none()
            && self.max_tool_calls.is_none()
    }
}

/// `[judge]` — LLM rubric scoring of the final answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Natural-language rubric the judge scores the answer against.
    pub rubric: String,
    /// Case passes the judge check when `score >= min_score` (0..=1).
    #[serde(default = "default_min_score")]
    pub min_score: f64,
}

impl EvalCaseFile {
    /// Fail-fast structural validation. Returns a human-readable error
    /// mentioning the offending field.
    pub fn validate(&self) -> Result<(), String> {
        if !duduclaw_core::is_valid_agent_id(&self.case.name) {
            return Err(format!(
                "[case] name {:?} invalid: use 1-64 chars of [a-zA-Z0-9_-]",
                self.case.name
            ));
        }
        if !duduclaw_core::is_valid_agent_id(&self.case.agent) {
            return Err(format!(
                "[case] agent {:?} invalid: use 1-64 chars of [a-zA-Z0-9_-]",
                self.case.agent
            ));
        }
        if self.case.prompt.trim().is_empty() {
            return Err("[case] prompt must not be empty".into());
        }
        if !(1..=3600).contains(&self.case.timeout_secs) {
            return Err("[case] timeout_secs must be 1..=3600".into());
        }
        if !(1..=100).contains(&self.case.max_turns) {
            return Err("[case] max_turns must be 1..=100".into());
        }
        if let Some(t) = &self.case.transcript {
            validate_relative_path(t)
                .map_err(|e| format!("[case] transcript {t:?}: {e}"))?;
        }
        if let Some(re) = &self.expect.output_regex {
            regex::Regex::new(re)
                .map_err(|e| format!("[expect] output_regex does not compile: {e}"))?;
        }
        if let Some(j) = &self.judge {
            if j.enabled && j.rubric.trim().is_empty() {
                return Err("[judge] rubric must not be empty when enabled".into());
            }
            if !(0.0..=1.0).contains(&j.min_score) {
                return Err("[judge] min_score must be within 0.0..=1.0".into());
            }
        }
        let judge_active = self.judge.as_ref().map(|j| j.enabled).unwrap_or(false);
        if self.expect.is_empty() && !judge_active {
            return Err(
                "case defines no checks: add at least one [expect] assertion or an enabled [judge]"
                    .into(),
            );
        }
        Ok(())
    }

    /// Effective model for the run.
    pub fn model(&self) -> &str {
        self.case.model.as_deref().unwrap_or(DEFAULT_EVAL_MODEL)
    }
}

/// Reject absolute paths and parent-directory escapes in a case-relative
/// path (security gate fails closed — a case file must not be able to read
/// arbitrary files as its "transcript").
fn validate_relative_path(p: &str) -> Result<(), String> {
    let path = Path::new(p);
    if path.is_absolute() {
        return Err("absolute paths are not allowed".into());
    }
    let escapes = path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if escapes {
        return Err("`..` path components are not allowed".into());
    }
    Ok(())
}

/// Parse + validate one case file.
pub fn load_case(path: &Path) -> Result<EvalCaseFile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let parsed: EvalCaseFile = toml::from_str(&raw)
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    parsed
        .validate()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(parsed)
}

/// Discover case files under `root`.
///
/// - `root` is a `.toml` file → that single case.
/// - `root` is a directory → every `*.toml` below it (recursive), sorted for
///   deterministic run order.
pub fn discover_cases(root: &Path) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        if root.extension().and_then(|e| e.to_str()) != Some("toml") {
            return Err(format!("{} is not a .toml case file", root.display()));
        }
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(format!(
            "eval path {} does not exist (expected a case file or suite directory)",
            root.display()
        ));
    }
    let mut found = Vec::new();
    collect_toml(root, &mut found)?;
    found.sort();
    if found.is_empty() {
        return Err(format!("no *.toml case files under {}", root.display()));
    }
    Ok(found)
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_toml(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal(extra: &str) -> String {
        format!(
            "[case]\nname = \"t1\"\nagent = \"support-bot\"\nprompt = \"hi\"\n{extra}"
        )
    }

    #[test]
    fn parses_minimal_case_with_expect() {
        let toml = minimal("[expect]\noutput_contains = [\"hello\"]\n");
        let case: EvalCaseFile = toml::from_str(&toml).unwrap();
        case.validate().unwrap();
        assert_eq!(case.case.name, "t1");
        assert_eq!(case.case.timeout_secs, 180);
        assert_eq!(case.case.max_turns, 25);
        assert_eq!(case.model(), DEFAULT_EVAL_MODEL);
    }

    #[test]
    fn rejects_case_without_any_check() {
        let case: EvalCaseFile = toml::from_str(&minimal("")).unwrap();
        let err = case.validate().unwrap_err();
        assert!(err.contains("no checks"), "unexpected error: {err}");
    }

    #[test]
    fn judge_only_case_is_valid() {
        let toml = minimal("[judge]\nrubric = \"polite\"\n");
        let case: EvalCaseFile = toml::from_str(&toml).unwrap();
        case.validate().unwrap();
        let judge = case.judge.unwrap();
        assert!(judge.enabled);
        assert!((judge.min_score - 0.7).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_agent_name() {
        let toml =
            "[case]\nname = \"t\"\nagent = \"../evil\"\nprompt = \"x\"\n[judge]\nrubric = \"r\"\n";
        let case: EvalCaseFile = toml::from_str(toml).unwrap();
        assert!(case.validate().is_err());
    }

    #[test]
    fn rejects_bad_regex_and_score_and_transcript() {
        let bad_re = minimal("[expect]\noutput_regex = \"(\"\n");
        let case: EvalCaseFile = toml::from_str(&bad_re).unwrap();
        assert!(case.validate().unwrap_err().contains("output_regex"));

        let bad_score = minimal("[judge]\nrubric = \"r\"\nmin_score = 1.5\n");
        let case: EvalCaseFile = toml::from_str(&bad_score).unwrap();
        assert!(case.validate().unwrap_err().contains("min_score"));

        let escape = "[case]\nname = \"t\"\nagent = \"a\"\nprompt = \"x\"\ntranscript = \"../../etc/passwd\"\n[judge]\nrubric = \"r\"\n";
        let case: EvalCaseFile = toml::from_str(escape).unwrap();
        assert!(case.validate().unwrap_err().contains(".."));

        let abs = "[case]\nname = \"t\"\nagent = \"a\"\nprompt = \"x\"\ntranscript = \"/etc/passwd\"\n[judge]\nrubric = \"r\"\n";
        let case: EvalCaseFile = toml::from_str(abs).unwrap();
        assert!(case.validate().unwrap_err().contains("absolute"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let toml = minimal("[expect]\nno_such_assertion = true\n");
        assert!(toml::from_str::<EvalCaseFile>(&toml).is_err());
    }

    #[test]
    fn discovers_cases_recursively_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("suite-a");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.toml"), "x").unwrap();
        std::fs::write(dir.path().join("a.toml"), "x").unwrap();
        std::fs::write(dir.path().join("notes.md"), "x").unwrap();
        let found = discover_cases(dir.path()).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("a.toml"));
        assert!(found[1].ends_with("b.toml"));
    }

    #[test]
    fn discover_errors_on_empty_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_cases(dir.path()).is_err());
        assert!(discover_cases(&dir.path().join("nope")).is_err());
    }
}
