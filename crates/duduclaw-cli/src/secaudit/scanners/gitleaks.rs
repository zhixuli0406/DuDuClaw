//! gitleaks JSON normalizer.
//!
//! Both scan modes (`gitleaks detect` for git history on repos, `gitleaks
//! dir` for plain directories — mode picked in `scanners::mod::gitleaks_args`)
//! emit the same top-level JSON array of leak objects via `--report-format
//! json`. `gitleaks` has no severity concept of its own —
//! every hit is a plausible secret, mapped to `Severity::High` uniformly
//! (a live/matched-pattern credential is always worth immediate attention;
//! see known limitations in the module doc for why this isn't finer-grained).
//!
//! **Secret handling**: `Secret`/`Match` in the raw gitleaks record contain
//! the actual matched credential text. That value is NEVER carried into the
//! `Finding.snippet` verbatim — it goes through
//! `duduclaw_security::audit::mask_sensitive_text` plus an explicit
//! byte-for-byte strip of the raw `Secret` value as a belt-and-suspenders
//! second pass, so a scan report can never leak the credential it found.

use serde::Deserialize;

use crate::secaudit::schema::{EvidenceItem, EvidenceKind, Finding, FindingKind, Severity};

pub const ENGINE_NAME: &str = "gitleaks";

/// One entry of gitleaks's JSON report array. `#[serde(default)]` on every
/// field: scanner output is untrusted DATA, a missing/renamed field must
/// degrade gracefully rather than fail the whole parse.
#[derive(Debug, Deserialize)]
struct GitleaksEntry {
    #[serde(default, rename = "Description")]
    description: String,
    #[serde(default, rename = "File")]
    file: String,
    #[serde(default, rename = "StartLine")]
    start_line: Option<u32>,
    #[serde(default, rename = "Match")]
    r#match: String,
    #[serde(default, rename = "Secret")]
    secret: String,
    #[serde(default, rename = "RuleID")]
    rule_id: String,
}

/// Mask the raw matched line so the actual secret material never survives
/// into a report. Two passes: the platform's general credential-shape
/// masker, then an explicit literal replace of the exact `secret` value (in
/// case it doesn't match any of the general masker's recognized shapes,
/// e.g. a bare high-entropy token with no recognizable key= prefix).
fn redact_match(raw_match: &str, secret: &str) -> String {
    let masked = duduclaw_security::audit::mask_sensitive_text(raw_match);
    if secret.is_empty() {
        masked
    } else {
        masked.replace(secret, "***")
    }
}

/// Parse a gitleaks JSON report (`--report-format json`) into findings.
/// `parse_error`-style failures (malformed top-level JSON) are surfaced to
/// the caller as `Err`, which the orchestrator records as `EngineRun.
/// parse_error` — never a panic. An empty array (`[]`, "no leaks found")
/// parses to an empty `Vec`, not an error.
pub fn parse(raw: &str) -> Result<Vec<Finding>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // Some gitleaks versions print nothing at all when there are zero
        // leaks and `--exit-code 0` — treat as "no findings", not an error.
        return Ok(Vec::new());
    }
    let entries: Vec<GitleaksEntry> =
        serde_json::from_str(trimmed).map_err(|e| format!("gitleaks JSON parse failed: {e}"))?;

    Ok(entries
        .into_iter()
        .map(|e| {
            let redacted = redact_match(&e.r#match, &e.secret);
            let title = if e.description.is_empty() {
                format!("Potential secret ({})", if e.rule_id.is_empty() { "unknown rule" } else { &e.rule_id })
            } else {
                e.description.clone()
            };
            let evidence = vec![EvidenceItem {
                kind: EvidenceKind::StaticHit,
                source: ENGINE_NAME.to_string(),
                detail: duduclaw_core::truncate_bytes(
                    &format!("gitleaks rule {}", if e.rule_id.is_empty() { "(unknown)" } else { &e.rule_id }),
                    crate::secaudit::schema::SNIPPET_MAX_BYTES,
                )
                .to_string(),
                recorded_at: chrono::Utc::now().to_rfc3339(),
            }];
            Finding::candidate(
                ENGINE_NAME,
                FindingKind::Secret,
                Severity::High,
                title,
                e.file,
                e.start_line,
                &redacted,
                if e.rule_id.is_empty() { "unknown".to_string() } else { e.rule_id },
                evidence,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Small realistic fixture mirroring gitleaks's real JSON report shape.
    const FIXTURE: &str = r#"[
      {
        "Description": "Generic API Key",
        "StartLine": 12,
        "EndLine": 12,
        "StartColumn": 5,
        "EndColumn": 40,
        "Match": "api_key = \"AKIAABCDEFGHIJKLMNOP\"",
        "Secret": "AKIAABCDEFGHIJKLMNOP",
        "File": "config.py",
        "SymlinkFile": "",
        "Commit": "abcd1234deadbeef",
        "Entropy": 4.2,
        "Author": "alice",
        "Email": "alice@example.com",
        "Date": "2026-01-01T00:00:00Z",
        "Message": "add config",
        "Tags": [],
        "RuleID": "generic-api-key",
        "Fingerprint": "abcd1234:config.py:generic-api-key:12"
      }
    ]"#;

    #[test]
    fn parses_realistic_fixture_into_one_finding() {
        let findings = parse(FIXTURE).unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.source_engine, "gitleaks");
        assert_eq!(f.kind, FindingKind::Secret);
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.file, "config.py");
        assert_eq!(f.line, Some(12));
        assert_eq!(f.rule_id, "generic-api-key");
        assert_eq!(f.title, "Generic API Key");
    }

    #[test]
    fn secret_value_never_appears_in_snippet_or_evidence() {
        let findings = parse(FIXTURE).unwrap();
        let f = &findings[0];
        assert!(!f.snippet.contains("AKIAABCDEFGHIJKLMNOP"));
        for ev in &f.evidence {
            assert!(!ev.detail.contains("AKIAABCDEFGHIJKLMNOP"));
        }
    }

    #[test]
    fn empty_array_is_zero_findings() {
        assert_eq!(parse("[]").unwrap().len(), 0);
    }

    #[test]
    fn empty_stdout_is_zero_findings_not_error() {
        assert_eq!(parse("").unwrap().len(), 0);
        assert_eq!(parse("   \n").unwrap().len(), 0);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse("{not valid json").is_err());
        assert!(parse("not json at all").is_err());
    }

    #[test]
    fn missing_optional_fields_degrade_gracefully() {
        let raw = r#"[{"File": "a.py"}]"#;
        let findings = parse(raw).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "unknown");
        assert!(findings[0].line.is_none());
        assert!(findings[0].title.contains("unknown rule"));
    }

    #[test]
    fn unexpected_shape_is_an_error_not_a_panic() {
        // Top-level object instead of array — gitleaks always emits an
        // array, so this is a defensive "shape drifted" guard.
        assert!(parse(r#"{"File": "a.py"}"#).is_err());
    }
}
