//! R8t: `REFUSAL_REASONS` is the gateway's allow-list for the worker's single
//! stderr line, so it must list every fixed error literal the worker can
//! return. This test scans the source text (not the compiled code) of
//! `src/{lib,linux,sealed,vendor}.rs`.
//!
//! Recognised error-producing forms (whitespace between tokens is free):
//!   `Err("…")`, `.map_err(|_| "…")`, `.ok_or("…")`, `.ok_or_else(|| "…")`.
//! Every other use of `Err(`, `map_err(`, `ok_or(` or `ok_or_else(` outside
//! comments and string literals (e.g. `Err(reason)`, `.map_err(|e| …)`,
//! `.ok_or(CONST)`) FAILS the test as an unrecognised form: extend the
//! scanner and the list instead of silently skipping it. Raw string literals
//! are refused by the scanner for the same reason.
//!
//! Limitation: an error produced in another crate and propagated with `?`
//! is not visible here (today every `?` operand in `linux::run()` is a
//! function of this crate with a `&'static str` error).
use duduclaw_api_worker::{REFUSAL_LINE_PREFIX, REFUSAL_REASONS};

const SOURCES: &[(&str, &str)] = &[
    ("lib.rs", include_str!("../src/lib.rs")),
    ("linux.rs", include_str!("../src/linux.rs")),
    ("sealed.rs", include_str!("../src/sealed.rs")),
    ("vendor.rs", include_str!("../src/vendor.rs")),
];
const MAIN: &str = include_str!("../src/main.rs");

/// Strips comments, replaces every string literal by `"@<index>"` and
/// returns that skeleton with the literals' raw contents.
fn lex(source: &str) -> Result<(String, Vec<String>), String> {
    let mut skeleton = String::new();
    let mut literals = Vec::new();
    let mut i = 0;
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    while i < source.len() {
        let rest = &source[i..];
        let previous = source[..i].chars().next_back();
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            skeleton.push(' ');
            continue;
        }
        if rest.starts_with("/*") {
            let end = rest.find("*/").map_or(rest.len(), |n| n + 2);
            skeleton.push(' ');
            skeleton.extend(rest[..end].chars().filter(|c| *c == '\n'));
            i += end;
            continue;
        }
        if (rest.starts_with("r\"") || rest.starts_with("r#\"") || rest.starts_with("br\"") || rest.starts_with("br#\""))
            && !previous.is_some_and(ident) {
            return Err(format!("raw string literal at byte {i}: extend the scanner"));
        }
        if rest.starts_with('"') {
            let mut end = None;
            let mut escaped = false;
            for (offset, c) in rest.char_indices().skip(1) {
                if escaped { escaped = false; continue; }
                match c { '\\' => escaped = true, '"' => { end = Some(offset); break; } _ => {} }
            }
            let end = end.ok_or_else(|| format!("unterminated string at byte {i}"))?;
            skeleton.push_str(&format!("\"@{}\"", literals.len()));
            literals.push(rest[1..end].to_owned());
            i += end + 1;
            continue;
        }
        if rest.starts_with('\'') {
            // Char literal ('x', '\n', '\'') or a lifetime ('a, 'static).
            let mut chars = rest.char_indices().skip(1);
            let literal_end = match chars.next() {
                Some((_, '\\')) => rest[2..].find('\'').map(|n| n + 2 + 1),
                Some((offset, c)) if rest[offset + c.len_utf8()..].starts_with('\'') => Some(offset + c.len_utf8() + 1),
                _ => None,
            };
            if let Some(end) = literal_end {
                // `'\''`: the escaped quote is not the terminator.
                let end = if rest.starts_with("'\\''") { 4 } else { end };
                skeleton.push_str("'_'");
                i += end;
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        skeleton.push(c);
        i += c.len_utf8();
    }
    Ok((skeleton, literals))
}

/// `(head, closure)`: after `head` (and `closure`, when not empty) the only
/// accepted argument is one string literal followed by `)`.
const FORMS: &[(&str, &str)] = &[("Err(", ""), ("map_err(", "|_|"), ("ok_or(", ""), ("ok_or_else(", "||")];

/// Every error literal produced by a recognised form; any other use of a
/// form head is an error naming the file and line.
fn error_literals(name: &str, source: &str) -> Result<Vec<String>, String> {
    let (skeleton, literals) = lex(source)?;
    let mut found = Vec::new();
    for (head, closure) in FORMS {
        for (position, _) in skeleton.match_indices(head) {
            if skeleton[..position].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') { continue; }
            let line = skeleton[..position].matches('\n').count() + 1;
            let unrecognised = || format!("{name}:{line} (approximate): unrecognised error-producing form `{head}…`");
            let rest = skeleton[position + head.len()..].trim_start();
            let rest = rest.strip_prefix(closure).ok_or_else(unrecognised)?.trim_start();
            let rest = rest.strip_prefix("\"@").ok_or_else(unrecognised)?;
            let digits = rest.find('"').ok_or_else(unrecognised)?;
            let index: usize = rest[..digits].parse().map_err(|_| unrecognised())?;
            if !rest[digits + 1..].trim_start().starts_with(')') { return Err(unrecognised()); }
            found.push(literals.get(index).ok_or_else(unrecognised)?.clone());
        }
    }
    Ok(found)
}

#[test]
fn every_worker_error_literal_is_an_allowed_refusal_reason() {
    let mut all = Vec::new();
    for (name, source) in SOURCES {
        let found = error_literals(name, source).unwrap_or_else(|error| panic!("{error}"));
        assert!(!found.is_empty(), "{name}: the scanner found no error literal");
        for literal in &found {
            assert!(REFUSAL_REASONS.contains(&literal.as_str()),
                "{name}: error literal {literal:?} is missing from REFUSAL_REASONS");
        }
        all.extend(found);
    }
    // Not vacuous: today there are well over 80 distinct reasons.
    assert!(all.len() > 80, "{}", all.len());
    // No stale entry: each is produced by a scanned error form, or is
    // main.rs's fixed non-Linux reason.
    for reason in REFUSAL_REASONS {
        assert!(all.iter().any(|literal| literal == reason) || MAIN.contains(&format!("\"{reason}\"")),
            "REFUSAL_REASONS entry {reason:?} is not produced by the worker");
    }
}

#[test]
fn refusal_reasons_are_unique_single_line_printable_ascii() {
    let mut seen = std::collections::BTreeSet::new();
    for reason in REFUSAL_REASONS {
        assert!(seen.insert(*reason), "duplicate {reason:?}");
        assert!(!reason.is_empty() && reason.len() <= 64, "{reason:?}");
        assert!(reason.bytes().all(|b| (0x20..0x7f).contains(&b)) && !reason.contains('\\'), "{reason:?}");
        assert_eq!(reason.trim(), *reason);
    }
    // The worker's single stderr line is exactly prefix + reason.
    assert!(MAIN.contains(&format!("eprintln!(\"{REFUSAL_LINE_PREFIX}{{reason}}\");")));
    assert!(MAIN.contains("std::process::exit(125);"));
}

#[test]
fn scanner_refuses_unrecognised_error_forms() {
    for bad in ["fn f() -> R { Err(reason) }", "x.map_err(|e| \"a\")", "x.ok_or(CONST)",
        "x.ok_or_else(|| make())", "x.map_err(|_| \"a\".into())", "y = r\"raw\";"] {
        assert!(error_literals("snippet", bad).is_err(), "{bad}");
    }
    let good = "// Err(ignored)\n/* ok_or(x) */ let s = \"Err(x)\"; let c = '\"'; fn f<'a>() { \
        x.map_err(|_| \"one\")?.ok_or( \"two\" )?; return Err(\"three\"); y.ok_or_else(|| \"four\"); z.is_err(); }";
    assert_eq!(error_literals("snippet", good).unwrap(), ["three", "one", "two", "four"]);
}
