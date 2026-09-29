//! Keeps `config/duduclaw.example.toml` honest.
//!
//! The 2026-09-29 feature audit (K3) found the shipped example covered roughly
//! 10 of the 50+ `config.toml` sections the code actually reads, so an operator
//! could not learn from it that `[tick]`, `[goal_loop]`, `[redaction]`,
//! `[mail]` or `[limits]` even existed. Filling it in only helps if it stays
//! filled in and stays correct, so this file asserts three things a reviewer
//! cannot eyeball reliably:
//!
//! 1. The file as shipped is valid TOML (it is copied to `~/.duduclaw/config.toml`).
//! 2. Every commented-out section header and `key = value` line would still be
//!    valid TOML once uncommented — the whole point of documenting a default is
//!    that a reader can uncomment it and have it work. Lines are checked one at
//!    a time rather than as one document, because the same section legitimately
//!    appears in more than one commented block and TOML forbids redefining a
//!    table.
//! 3. The sections the audit specifically called out are present. A future edit
//!    that drops one fails here instead of silently regressing coverage.
//!
//! Deliberately NOT asserted: that every section the codebase reads appears
//! here. There is no single `Config` struct to enumerate from — sections are
//! parsed in isolation by ~40 separate readers — so such a check would have to
//! hard-code the very list it is supposed to verify.

use std::path::PathBuf;

fn example_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is <repo>/crates/duduclaw-core.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/duduclaw.example.toml")
        .canonicalize()
        .expect("config/duduclaw.example.toml exists at the repo root")
}

fn example_text() -> String {
    std::fs::read_to_string(example_path()).expect("example config is readable")
}

/// Strip one leading `#` (and at most one following space) from a comment line.
/// Returns `None` for a line that is not a comment.
fn uncomment(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('#')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// `[section]` / `[[array.of.tables]]`, optionally followed by a comment.
fn looks_like_table_header(body: &str) -> bool {
    let b = body.trim_end();
    let Some(open) = b.strip_prefix('[') else {
        return false;
    };
    let inner = open.strip_prefix('[').unwrap_or(open);
    let Some(close) = inner.find(']') else {
        return false;
    };
    let name = &inner[..close];
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '"'))
}

/// First dotted segment of a table header — `mcp_keys."abc"` ⇒ `mcp_keys`.
fn top_level_section(body: &str) -> Option<&str> {
    let inner = body.trim_start_matches('[');
    let close = inner.find(']')?;
    Some(inner[..close].split('.').next().unwrap_or(""))
}

/// `bare_key = <something>`, optionally followed by a comment. Deliberately
/// strict about the left-hand side so prose such as
/// "…which is over max_in_window = 20" is not mistaken for a setting.
fn looks_like_key_value(body: &str) -> bool {
    let Some(eq) = body.find('=') else {
        return false;
    };
    let key = body[..eq].trim_end();
    let value = body[eq + 1..].trim_start();
    !key.is_empty()
        && !value.is_empty()
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        && body.starts_with(key)
}

#[test]
fn example_config_file_is_valid_toml() {
    let text = example_text();
    text.parse::<toml::Table>()
        .expect("config/duduclaw.example.toml must parse as TOML exactly as shipped");
}

#[test]
fn example_config_documented_defaults_would_parse_if_uncommented() {
    let text = example_text();
    let mut checked = 0usize;
    for (n, line) in text.lines().enumerate() {
        let Some(body) = uncomment(line) else { continue };
        let is_header = looks_like_table_header(body);
        if !is_header && !looks_like_key_value(body) {
            continue;
        }
        checked += 1;
        // A header needs a body to be a legal document on its own; a key line
        // already is one.
        let doc = if is_header {
            format!("{body}\n")
        } else {
            body.to_owned()
        };
        assert!(
            doc.parse::<toml::Table>().is_ok(),
            "line {}: uncommenting `{}` would not parse as TOML",
            n + 1,
            body.trim()
        );
    }
    assert!(
        checked >= 100,
        "expected the example to document at least 100 settings; found {checked} \
         (did a section get deleted?)"
    );
}

#[test]
fn example_config_covers_the_sections_the_audit_named() {
    let text = example_text();
    // Present either live (uncommented) or documented as `# [name]`.
    let mut seen: Vec<String> = Vec::new();
    for line in text.lines() {
        let body = uncomment(line).unwrap_or(line);
        if !looks_like_table_header(body) {
            continue;
        }
        if let Some(name) = top_level_section(body) {
            seen.push(name.to_owned());
        }
    }
    // The 2026-09-29 audit's "first-class features absent from the example"
    // list, plus the sections this wave added a switch for.
    for required in [
        "general",
        "gateway",
        "dispatch",
        "dispatch_guard",
        "goal_loop",
        "goal_defaults",
        "goal_intent",
        "memory",
        "channels",
        "notify",
        "redaction",
        "files",
        "mail",
        "backup",
        "dashboard",
        "delegation",
        "mcp_keys",
        "runtime",
        "telemetry",
        "api",
        "task_forward_model",
        "belief",
        "tick",
        "limits",
        "decision",
        "ccr",
        "causal_extraction",
        "evolution",
        "team",
    ] {
        assert!(
            seen.iter().any(|s| s == required),
            "config/duduclaw.example.toml no longer documents `[{required}]`"
        );
    }
}
