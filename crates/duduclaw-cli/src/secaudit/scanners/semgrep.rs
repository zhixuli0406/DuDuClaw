//! semgrep JSON normalizer + bundled offline ruleset.
//!
//! **Known limitation (documented per task instruction, not silently
//! worked around)**: semgrep's usual `--config auto` / `--config p/<pack>`
//! shortcuts fetch rules from the semgrep registry over the network (or
//! require `semgrep login`). `duduclaw secaudit` runs fully offline this
//! wave (§3.3 discipline + task requirement), so neither is usable. Instead
//! this module writes a small **bundled, local** rule file
//! ([`BUNDLED_RULES_YAML`]) to a temp path and invokes
//! `semgrep scan --json --config <local-path>` — a local filesystem path
//! never triggers a network fetch. The bundled ruleset is intentionally
//! minimal (a handful of generic-security patterns, not a registry-grade
//! ruleset) and has NOT been validated against a live semgrep install in
//! this sandbox (no network, and semgrep is not guaranteed installed here) —
//! only its YAML syntax is sanity-checked by a unit test. Treat it as a
//! starting point; deeper coverage requires running semgrep with the full
//! registry config online, out of scope for this offline CLI wave.

use std::path::PathBuf;

use serde::Deserialize;

use crate::secaudit::schema::{EvidenceItem, EvidenceKind, Finding, FindingKind, Severity};

pub const ENGINE_NAME: &str = "semgrep";

/// Minimal offline ruleset. Kept intentionally small (§ module doc) —
/// generic cross-language `eval`/shell-injection/unsafe-deserialization
/// patterns that don't require semgrep's typed dataflow engine to catch.
pub const BUNDLED_RULES_YAML: &str = r#"rules:
  - id: python-eval-exec-usage
    languages: [python]
    message: "Use of eval()/exec() on dynamic input can lead to arbitrary code execution."
    severity: ERROR
    patterns:
      - pattern-either:
          - pattern: eval(...)
          - pattern: exec(...)

  - id: python-subprocess-shell-true
    languages: [python]
    message: "subprocess call with shell=True can lead to command injection if input is not sanitized."
    severity: ERROR
    pattern: subprocess.$FUNC(..., shell=True, ...)

  - id: python-yaml-load-unsafe
    languages: [python]
    message: "yaml.load without a safe Loader can execute arbitrary code from untrusted YAML."
    severity: ERROR
    pattern: yaml.load($DATA)

  - id: javascript-eval-usage
    languages: [javascript, typescript]
    message: "Use of eval() can lead to arbitrary code execution."
    severity: ERROR
    pattern: eval(...)

  - id: javascript-child-process-exec
    languages: [javascript, typescript]
    message: "child_process.exec with dynamic input can lead to command injection; prefer execFile with an argument array."
    severity: WARNING
    pattern: require('child_process').exec(...)

  - id: generic-hardcoded-private-key
    languages: [generic]
    message: "Hardcoded private key material found in source."
    severity: ERROR
    pattern-regex: '-----BEGIN (RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----'
"#;

/// Temp-file guard for the bundled ruleset (mirrors `eval::runner::TempPromptFile`'s
/// convention: `std::env::temp_dir()` + a uuid-suffixed filename + best-effort
/// cleanup on drop). Not a secret, but shouldn't accumulate in tmp.
pub struct TempRulesFile(pub PathBuf);

impl TempRulesFile {
    pub fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "duduclaw-secaudit-semgrep-rules-{}.yml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, BUNDLED_RULES_YAML)
            .map_err(|e| format!("cannot write bundled semgrep rules to temp file: {e}"))?;
        Ok(TempRulesFile(path))
    }
}

impl Drop for TempRulesFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// semgrep `--json` top-level shape (subset — only what we use).
#[derive(Debug, Deserialize)]
struct SemgrepReport {
    #[serde(default)]
    results: Vec<SemgrepResult>,
}

#[derive(Debug, Deserialize)]
struct SemgrepResult {
    #[serde(default)]
    check_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    start: Option<SemgrepPos>,
    #[serde(default)]
    extra: Option<SemgrepExtra>,
}

#[derive(Debug, Deserialize)]
struct SemgrepPos {
    #[serde(default)]
    line: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SemgrepExtra {
    #[serde(default)]
    message: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    lines: String,
}

/// semgrep's `extra.severity` is one of `ERROR`/`WARNING`/`INFO` (not the
/// CRITICAL/HIGH/MEDIUM/LOW/INFO scale some registry rule metadata uses).
/// Mapped conservatively: ERROR→High, WARNING→Medium, INFO→Low, anything
/// else (unrecognized/absent) → Info, fail-open toward under- rather than
/// over-alarming for an unrecognized value.
fn map_severity(raw: &str) -> Severity {
    match raw.to_ascii_uppercase().as_str() {
        "ERROR" => Severity::High,
        "WARNING" => Severity::Medium,
        "INFO" => Severity::Low,
        _ => Severity::Info,
    }
}

/// Parse `semgrep scan --json` output. Malformed top-level JSON is an
/// `Err`; missing/absent `results` degrades to zero findings (semgrep
/// always emits `results: []` when clean, but we don't rely on that).
pub fn parse(raw: &str) -> Result<Vec<Finding>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let report: SemgrepReport =
        serde_json::from_str(trimmed).map_err(|e| format!("semgrep JSON parse failed: {e}"))?;

    Ok(report
        .results
        .into_iter()
        .map(|r| {
            let extra = r.extra.unwrap_or(SemgrepExtra {
                message: String::new(),
                severity: String::new(),
                lines: String::new(),
            });
            let severity = map_severity(&extra.severity);
            let line = r.start.and_then(|s| s.line);
            let title = if extra.message.is_empty() {
                format!("semgrep rule {}", r.check_id)
            } else {
                extra.message.clone()
            };
            let evidence = vec![EvidenceItem {
                kind: EvidenceKind::StaticHit,
                source: ENGINE_NAME.to_string(),
                detail: duduclaw_core::truncate_bytes(&extra.message, crate::secaudit::schema::SNIPPET_MAX_BYTES)
                    .to_string(),
                recorded_at: chrono::Utc::now().to_rfc3339(),
            }];
            Finding::candidate(
                ENGINE_NAME,
                FindingKind::StaticAnalysis,
                severity,
                title,
                r.path,
                line,
                &extra.lines,
                r.check_id,
                evidence,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "results": [
        {
          "check_id": "python-eval-exec-usage",
          "path": "app.py",
          "start": {"line": 10, "col": 1, "offset": 100},
          "end": {"line": 10, "col": 20, "offset": 120},
          "extra": {
            "message": "Use of eval()/exec() on dynamic input can lead to arbitrary code execution.",
            "severity": "ERROR",
            "lines": "eval(user_input)",
            "metadata": {"cwe": ["CWE-95"]}
          }
        },
        {
          "check_id": "javascript-child-process-exec",
          "path": "server.js",
          "start": {"line": 42},
          "extra": {
            "message": "child_process.exec with dynamic input.",
            "severity": "WARNING",
            "lines": "exec(cmd)"
          }
        }
      ],
      "errors": [],
      "paths": {"scanned": ["app.py", "server.js"], "skipped": []}
    }"#;

    #[test]
    fn parses_realistic_fixture_into_findings() {
        let findings = parse(FIXTURE).unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].file, "app.py");
        assert_eq!(findings[0].line, Some(10));
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].rule_id, "python-eval-exec-usage");
        assert_eq!(findings[1].severity, Severity::Medium);
    }

    #[test]
    fn severity_mapping_covers_all_three_levels_and_unknown() {
        assert_eq!(map_severity("ERROR"), Severity::High);
        assert_eq!(map_severity("error"), Severity::High);
        assert_eq!(map_severity("WARNING"), Severity::Medium);
        assert_eq!(map_severity("INFO"), Severity::Low);
        assert_eq!(map_severity("SOMETHING_NEW"), Severity::Info);
        assert_eq!(map_severity(""), Severity::Info);
    }

    #[test]
    fn empty_results_is_zero_findings() {
        let findings = parse(r#"{"results": [], "errors": []}"#).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn empty_stdout_is_zero_findings_not_error() {
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse("{not json").is_err());
    }

    #[test]
    fn missing_extra_block_degrades_gracefully() {
        let raw = r#"{"results": [{"check_id": "x", "path": "a.py"}]}"#;
        let findings = parse(raw).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].line.is_none());
        assert!(findings[0].title.contains('x'));
    }

    #[test]
    fn bundled_ruleset_is_syntactically_valid_yaml() {
        // Sanity check only — this does NOT confirm semgrep accepts the
        // rule schema (that needs a live semgrep run, out of scope here).
        let parsed: serde_yaml::Value = serde_yaml::from_str(BUNDLED_RULES_YAML)
            .expect("bundled semgrep ruleset must be valid YAML");
        let rules = parsed.get("rules").expect("must have a top-level `rules` key");
        assert!(rules.as_sequence().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn temp_rules_file_writes_and_cleans_up() {
        let guard = TempRulesFile::create().unwrap();
        let path = guard.0.clone();
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, BUNDLED_RULES_YAML);
        drop(guard);
        assert!(!path.exists());
    }
}
