//! Pure helpers behind the dashboard's "我的規則" (custom rules) card.
//!
//! Three independent, side-effect-free pieces, all deliberately in the
//! redaction crate rather than the gateway so they can be unit-tested without
//! a home directory, a config file or a tokio runtime:
//!
//! 1. [`synthesize_example`] — turn a regex into ONE representative value.
//!    §13.1 of the design forbids storing the operator's real examples, so the
//!    list row's "代表值" is synthesised from the pattern instead of
//!    remembered from the form.
//! 2. [`suggest_pattern_heuristic`] — turn 2–5 example values into a regex
//!    without any model. This is the offline floor under
//!    `redaction.suggest_pattern`: local inference → cloud utility model →
//!    here.
//! 3. [`derive_category_id`] — turn a human label ("員工編號") into a token
//!    category that satisfies the `[A-Z0-9_]{1,32}` rule enforced by
//!    [`crate::token`].
//!
//! Nothing here logs, and nothing here retains its inputs.

use std::collections::HashMap;

use regex::RegexBuilder;
use regex_syntax::hir::{Class, Hir, HirKind};

/// Prefix every dashboard-created category carries.
pub const CUSTOM_CATEGORY_PREFIX: &str = "CUSTOM_";

/// Hard cap from [`crate::token`] — a category longer than this can never
/// produce a valid token.
pub const CATEGORY_MAX_LEN: usize = 32;

/// Longest synthesised example we are willing to render. A pattern that would
/// expand past this (`a{500}`) yields `None` instead of a wall of text.
const EXAMPLE_MAX_CHARS: usize = 128;

/// Sample length used for an unbounded repetition (`+` / `*` / `{2,}`).
const UNBOUNDED_REPEAT_SAMPLE: u32 = 4;

/// `regex` compile size limit applied to every operator-supplied pattern
/// (1 MiB). Matches §13.2.
pub const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// Maximum characters in an operator-supplied pattern (§13.2).
pub const PATTERN_MAX_CHARS: usize = 512;

// ═══════════════════════════════════════════════════════════════════════
// 1. Example synthesis — regex → one representative value
// ═══════════════════════════════════════════════════════════════════════

/// Synthesise a representative value for `pattern`.
///
/// Rules (design §13.1): a literal is copied verbatim, a character class
/// contributes one representative character, a repetition contributes its
/// minimum count (or [`UNBOUNDED_REPEAT_SAMPLE`] when it has no upper bound),
/// an alternation contributes its first branch, and anchors / look-arounds
/// contribute nothing.
///
/// Returns `None` when the pattern does not parse, when it would expand past
/// [`EXAMPLE_MAX_CHARS`], or when the result is empty — the caller then shows
/// the pattern itself, which is honest rather than wrong.
pub fn synthesize_example(pattern: &str) -> Option<String> {
    let hir = regex_syntax::parse(pattern).ok()?;
    let mut out = String::new();
    render_hir(&hir, &mut out)?;
    if out.is_empty() { None } else { Some(out) }
}

fn render_hir(hir: &Hir, out: &mut String) -> Option<()> {
    if out.chars().count() > EXAMPLE_MAX_CHARS {
        return None;
    }
    match hir.kind() {
        HirKind::Empty => Some(()),
        // Anchors and word boundaries match the empty string — they shape
        // *where* a match may start, never what it contains.
        HirKind::Look(_) => Some(()),
        HirKind::Literal(lit) => {
            // A literal is bytes in the HIR; a pattern written as UTF-8 text
            // always yields valid UTF-8 here, and a byte-oriented one that
            // does not is a case we decline rather than mangle.
            let s = std::str::from_utf8(&lit.0).ok()?;
            out.push_str(s);
            Some(())
        }
        HirKind::Class(class) => {
            out.push(class_sample(class)?);
            Some(())
        }
        HirKind::Capture(cap) => render_hir(&cap.sub, out),
        HirKind::Concat(parts) => {
            for p in parts {
                render_hir(p, out)?;
            }
            Some(())
        }
        // First branch: deterministic, and the one an operator reading the
        // pattern left-to-right expects to see.
        HirKind::Alternation(branches) => render_hir(branches.first()?, out),
        HirKind::Repetition(rep) => {
            let count = if rep.max.is_none() {
                rep.min.max(UNBOUNDED_REPEAT_SAMPLE)
            } else {
                rep.min
            };
            if count as usize > EXAMPLE_MAX_CHARS {
                return None;
            }
            for _ in 0..count {
                render_hir(&rep.sub, out)?;
                if out.chars().count() > EXAMPLE_MAX_CHARS {
                    return None;
                }
            }
            Some(())
        }
    }
}

/// One representative character for a class.
///
/// Prefers `0`, then `A`, then `a` when the class admits them (so `\d` reads
/// as `0` and `[A-Z]` as `A`), then the first printable ASCII character in the
/// class, and only as a last resort the class's very first codepoint. The
/// preference exists because a negated class like `[^0-9]` starts at `\0`,
/// and a NUL in the dashboard's "代表值" column would be worse than useless.
fn class_sample(class: &Class) -> Option<char> {
    // Bound the scan per range: a class range can span the whole Unicode
    // space, and we only ever want a character near its start.
    const SCAN: u32 = 128;

    let mut ranges: Vec<(char, char)> = Vec::new();
    match class {
        Class::Unicode(u) => ranges.extend(u.iter().map(|r| (r.start(), r.end()))),
        Class::Bytes(b) => ranges.extend(
            b.iter()
                .filter_map(|r| Some((char::from(r.start()), char::from(r.end())))),
        ),
    }

    let mut fallback = None;
    for (lo, hi) in ranges {
        if fallback.is_none() {
            fallback = Some(lo);
        }
        for probe in ['0', 'A', 'a'] {
            if lo <= probe && probe <= hi {
                return Some(probe);
            }
        }
        let start = lo as u32;
        let end = (hi as u32).min(start.saturating_add(SCAN));
        for cp in start..=end {
            if let Some(c) = char::from_u32(cp)
                && c.is_ascii_graphic()
            {
                return Some(c);
            }
        }
    }
    fallback
}

// ═══════════════════════════════════════════════════════════════════════
// 2. Heuristic pattern suggestion — examples → regex, no model
// ═══════════════════════════════════════════════════════════════════════

/// One aligned segment of an example value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Digit,
    Upper,
    Lower,
    /// Mixed-case ASCII letters (or two examples that disagree on case).
    Letter,
    /// Any ASCII-alphanumeric run — the loosened second pass.
    Alnum,
    /// A single non-alphanumeric character, matched verbatim.
    Literal(char),
}

#[derive(Debug, Clone)]
struct Run {
    kind: RunKind,
    len: usize,
}

/// Split `value` into runs. `merge_alnum` selects the second (loose) pass,
/// where letters and digits are not distinguished.
fn split_runs(value: &str, merge_alnum: bool) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for ch in value.chars() {
        let kind = if merge_alnum {
            if ch.is_ascii_alphanumeric() {
                Some(RunKind::Alnum)
            } else {
                None
            }
        } else if ch.is_ascii_digit() {
            Some(RunKind::Digit)
        } else if ch.is_ascii_uppercase() {
            Some(RunKind::Upper)
        } else if ch.is_ascii_lowercase() {
            Some(RunKind::Lower)
        } else {
            None
        };
        match kind {
            // A non-alphanumeric character is always its own run: it is the
            // separator the alignment check keys on.
            None => runs.push(Run {
                kind: RunKind::Literal(ch),
                len: 1,
            }),
            Some(k) => match runs.last_mut() {
                Some(last) if last.kind == k => last.len += 1,
                _ => runs.push(Run { kind: k, len: 1 }),
            },
        }
    }
    runs
}

/// Can two run kinds at the same position be reconciled, and into what?
fn unify(a: RunKind, b: RunKind) -> Option<RunKind> {
    use RunKind::*;
    match (a, b) {
        (x, y) if x == y => Some(x),
        // Case differences across examples widen to "a letter".
        (Upper, Lower)
        | (Lower, Upper)
        | (Letter, Upper)
        | (Upper, Letter)
        | (Letter, Lower)
        | (Lower, Letter) => Some(Letter),
        // Everything else (a digit where another example has a letter, or two
        // different separators) means the examples are not the same shape.
        _ => None,
    }
}

/// Render one aligned run as a regex fragment.
fn render_run(kind: RunKind, min: usize, max: usize) -> String {
    let class = match kind {
        RunKind::Digit => r"\d",
        RunKind::Upper => "[A-Z]",
        RunKind::Lower => "[a-z]",
        RunKind::Letter => "[A-Za-z]",
        RunKind::Alnum => "[A-Za-z0-9]",
        RunKind::Literal(c) => return regex::escape(&c.to_string()),
    };
    if min == max {
        format!("{class}{{{min}}}")
    } else {
        format!("{class}{{{min},{max}}}")
    }
}

/// Align one pass's run sequences into a pattern, or `None` when the examples
/// do not share a shape.
fn align_pass(examples: &[String], merge_alnum: bool) -> Option<String> {
    let tokenised: Vec<Vec<Run>> = examples
        .iter()
        .map(|e| split_runs(e, merge_alnum))
        .collect();
    let first = tokenised.first()?;
    if first.is_empty() {
        return None;
    }
    if tokenised.iter().any(|t| t.len() != first.len()) {
        return None;
    }

    let mut kinds: Vec<RunKind> = first.iter().map(|r| r.kind).collect();
    let mut mins: Vec<usize> = first.iter().map(|r| r.len).collect();
    let mut maxs: Vec<usize> = mins.clone();
    for runs in tokenised.iter().skip(1) {
        for (i, run) in runs.iter().enumerate() {
            kinds[i] = unify(kinds[i], run.kind)?;
            mins[i] = mins[i].min(run.len);
            maxs[i] = maxs[i].max(run.len);
        }
    }

    let mut pattern = String::new();
    for i in 0..kinds.len() {
        pattern.push_str(&render_run(kinds[i], mins[i], maxs[i]));
    }
    Some(pattern)
}

/// Does `pattern` match `value` in full (anchored)?
pub fn matches_fully(pattern: &str, value: &str) -> bool {
    let anchored = format!("^(?:{pattern})$");
    build_pattern(&anchored).is_ok_and(|re| re.is_match(value))
}

/// Would `pattern` fire anywhere inside `value`? This is the question a
/// counter-example asks: the live engine searches, it does not anchor.
pub fn matches_anywhere(pattern: &str, value: &str) -> bool {
    build_pattern(pattern).is_ok_and(|re| re.is_match(value))
}

/// Compile an operator-supplied pattern under the shared size limit.
pub fn build_pattern(pattern: &str) -> Result<regex::Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
}

/// Does `pattern` satisfy every example and reject every counter-example?
pub fn pattern_satisfies(pattern: &str, examples: &[String], counter_examples: &[String]) -> bool {
    if build_pattern(pattern).is_err() {
        return false;
    }
    examples.iter().all(|e| matches_fully(pattern, e))
        && counter_examples
            .iter()
            .all(|c| !matches_anywhere(pattern, c))
}

/// Build a regex from example values with no model at all.
///
/// Two passes. The first keeps digits, upper-case and lower-case apart, which
/// gives a tight pattern for the common corporate-identifier shape
/// (`EMP-2024-0133`). If the examples do not share that shape, the second pass
/// treats every alphanumeric run as one class and tries again. Both passes are
/// verified before being returned: every example must match in full and no
/// counter-example may match anywhere. `None` means "no pattern I can stand
/// behind" — the caller must not invent one.
pub fn suggest_pattern_heuristic(
    examples: &[String],
    counter_examples: &[String],
) -> Option<String> {
    if examples.is_empty() {
        return None;
    }
    for merge_alnum in [false, true] {
        if let Some(pattern) = align_pass(examples, merge_alnum)
            && pattern_satisfies(&pattern, examples, counter_examples)
        {
            return Some(pattern);
        }
    }
    None
}

// ═══════════════════════════════════════════════════════════════════════
// 3. Category-id derivation
// ═══════════════════════════════════════════════════════════════════════

/// Is `id` a legal token category (`[A-Z0-9_]{1,32}`)?
pub fn is_valid_category(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= CATEGORY_MAX_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// ASCII slug of `label`, upper-cased, `_`-separated, no leading/trailing or
/// doubled separators. Empty when the label carries no ASCII alphanumerics
/// (e.g. a pure-CJK name) — that is the signal to fall back to a counter.
fn ascii_slug(label: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.push(ch.to_ascii_uppercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// Trim an id to [`CATEGORY_MAX_LEN`] without leaving a trailing separator.
/// The id is ASCII by construction here, so a char count is a byte count —
/// but it is still built by iterating chars, never sliced by byte index.
fn cap_category(id: &str) -> String {
    let mut out: String = id.chars().take(CATEGORY_MAX_LEN).collect();
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Derive the token category for a newly created custom rule.
///
/// `existing` is the profile's `[meta.labels]` map (category id → label).
/// An ASCII label becomes `CUSTOM_<SLUG>`; a label with no ASCII
/// alphanumerics becomes `CUSTOM_NN` with the lowest free two-digit counter.
/// Re-deriving for a label that already owns an id returns that same id, so
/// the call is idempotent; a slug that would collide with a *different*
/// label's id gains a numeric suffix.
///
/// The result always satisfies [`is_valid_category`].
pub fn derive_category_id(label: &str, existing: &HashMap<String, String>) -> String {
    let label = label.trim();

    // Idempotence first: an id already mapped to this exact label wins over
    // any freshly derived one, so editing a rule never renames its category.
    let mut reused: Vec<&String> = existing
        .iter()
        .filter(|(_, v)| v.trim() == label)
        .map(|(k, _)| k)
        .collect();
    reused.sort();
    if let Some(id) = reused.first() {
        return (*id).clone();
    }

    let slug = ascii_slug(label);
    if !slug.is_empty() {
        let base = cap_category(&format!("{CUSTOM_CATEGORY_PREFIX}{slug}"));
        if is_valid_category(&base) && !existing.contains_key(&base) {
            return base;
        }
        // Collision with a different label: widen with a numeric suffix,
        // trimming the base so the total stays inside the cap.
        for n in 2..=99u32 {
            let suffix = format!("_{n}");
            let keep = CATEGORY_MAX_LEN.saturating_sub(suffix.chars().count());
            let candidate = format!(
                "{}{suffix}",
                cap_category(&base.chars().take(keep).collect::<String>())
            );
            if is_valid_category(&candidate) && !existing.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    // Non-ASCII label (or an exhausted slug space): the two-digit counter.
    // Past 99 it simply grows a digit — still inside the 32-char cap.
    for n in 1..=9999u32 {
        let candidate = if n < 100 {
            format!("{CUSTOM_CATEGORY_PREFIX}{n:02}")
        } else {
            format!("{CUSTOM_CATEGORY_PREFIX}{n}")
        };
        if !existing.contains_key(&candidate) {
            return candidate;
        }
    }
    // Unreachable in practice (a profile is capped far below 9999 rules);
    // returning a valid-but-generic id beats panicking on a dashboard call.
    format!("{CUSTOM_CATEGORY_PREFIX}OVERFLOW")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. example synthesis ────────────────────────────────

    #[test]
    fn synthesizes_employee_id_shape() {
        assert_eq!(
            synthesize_example(r"EMP-\d{4}-\d{4}").as_deref(),
            Some("EMP-0000-0000")
        );
    }

    #[test]
    fn synthesizes_explicit_digit_class() {
        assert_eq!(
            synthesize_example("CU-[0-9]{6}").as_deref(),
            Some("CU-000000")
        );
    }

    #[test]
    fn unbounded_repetition_samples_four() {
        assert_eq!(
            synthesize_example(r"[A-Z]{2,4}\d+").as_deref(),
            Some("AA0000")
        );
    }

    #[test]
    fn anchors_contribute_nothing() {
        assert_eq!(
            synthesize_example(r"^\bTW-\d{3}\b$").as_deref(),
            Some("TW-000")
        );
    }

    #[test]
    fn alternation_takes_first_branch() {
        assert_eq!(
            synthesize_example(r"(?:AB|XY)-\d{2}").as_deref(),
            Some("AB-00")
        );
    }

    #[test]
    fn optional_group_contributes_nothing() {
        assert_eq!(synthesize_example(r"A\d?B").as_deref(), Some("AB"));
    }

    #[test]
    fn invalid_pattern_yields_none() {
        assert!(synthesize_example(r"[unclosed").is_none());
    }

    #[test]
    fn runaway_repetition_yields_none() {
        assert!(synthesize_example(r"a{500}").is_none());
    }

    #[test]
    fn negated_class_never_samples_a_control_character() {
        let s = synthesize_example(r"[^0-9]{3}").expect("negated class renders");
        assert!(
            s.chars().all(|c| c.is_ascii_graphic()),
            "expected printable sample, got {s:?}"
        );
    }

    #[test]
    fn synthesized_example_matches_its_own_pattern() {
        for pat in [r"EMP-\d{4}-\d{4}", "CU-[0-9]{6}", r"[A-Z]{2}\d{3}"] {
            let ex = synthesize_example(pat).expect("renders");
            assert!(
                matches_fully(pat, &ex),
                "pattern {pat} should match its own sample {ex}"
            );
        }
    }

    // ── 2. heuristic suggestion ─────────────────────────────

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn heuristic_learns_fixed_width_identifier() {
        let p = suggest_pattern_heuristic(&v(&["EMP-2024-0133", "EMP-2025-0007"]), &[])
            .expect("aligned");
        assert_eq!(p, r"[A-Z]{3}\-\d{4}\-\d{4}");
        assert!(matches_fully(&p, "EMP-2024-0133"));
        assert!(matches_fully(&p, "EMP-2025-0007"));
    }

    #[test]
    fn heuristic_widens_variable_length_runs() {
        let p = suggest_pattern_heuristic(&v(&["CU-1234", "CU-123456"]), &[]).expect("aligned");
        assert_eq!(p, r"[A-Z]{2}\-\d{4,6}");
    }

    #[test]
    fn heuristic_widens_case_disagreement_to_letters() {
        let p = suggest_pattern_heuristic(&v(&["AB-01", "ab-02"]), &[]).expect("aligned");
        assert_eq!(p, r"[A-Za-z]{2}\-\d{2}");
    }

    #[test]
    fn heuristic_falls_back_to_alnum_runs() {
        // "ABC" vs "AB1" disagree on run kind in the tight pass; the loose
        // pass sees one alphanumeric run on both sides.
        let p = suggest_pattern_heuristic(&v(&["ABC-01", "AB1-02"]), &[]).expect("loose pass");
        assert_eq!(p, r"[A-Za-z0-9]{3}\-[A-Za-z0-9]{2}");
        assert!(matches_fully(&p, "ABC-01"));
        assert!(matches_fully(&p, "AB1-02"));
    }

    #[test]
    fn heuristic_refuses_when_a_counter_example_matches() {
        // Same shape as the examples ⇒ every candidate pattern fires on it.
        assert!(
            suggest_pattern_heuristic(
                &v(&["EMP-2024-0133", "EMP-2025-0007"]),
                &v(&["EMP-1999-0001"])
            )
            .is_none()
        );
    }

    #[test]
    fn heuristic_accepts_a_distinguishable_counter_example() {
        let p = suggest_pattern_heuristic(
            &v(&["EMP-2024-0133", "EMP-2025-0007"]),
            &v(&["invoice 2024"]),
        )
        .expect("counter-example is a different shape");
        assert!(!matches_anywhere(&p, "invoice 2024"));
    }

    #[test]
    fn heuristic_refuses_structurally_unrelated_examples() {
        assert!(suggest_pattern_heuristic(&v(&["EMP-2024-0133", "台北市信義區"]), &[]).is_none());
    }

    #[test]
    fn heuristic_escapes_regex_metacharacters_in_separators() {
        let p = suggest_pattern_heuristic(&v(&["A.1", "B.2"]), &[]).expect("aligned");
        assert!(matches_fully(&p, "A.1"));
        // The '.' must be a literal, not "any char".
        assert!(!matches_fully(&p, "AX1"));
    }

    #[test]
    fn counter_example_check_is_unanchored() {
        // The live engine searches; a pattern that fires *inside* a
        // counter-example would still redact it.
        assert!(matches_anywhere(r"\d{4}", "order 2024 shipped"));
        assert!(!matches_fully(r"\d{4}", "order 2024 shipped"));
    }

    // ── 3. category-id derivation ───────────────────────────

    #[test]
    fn ascii_label_becomes_uppercase_slug() {
        let labels = HashMap::new();
        assert_eq!(
            derive_category_id("Employee ID", &labels),
            "CUSTOM_EMPLOYEE_ID"
        );
    }

    #[test]
    fn ascii_slug_is_capped_at_32_chars() {
        let labels = HashMap::new();
        let id = derive_category_id("a very long employee identifier label", &labels);
        assert!(id.chars().count() <= CATEGORY_MAX_LEN, "{id}");
        assert!(is_valid_category(&id), "{id}");
        assert!(!id.ends_with('_'), "{id}");
    }

    #[test]
    fn cjk_label_uses_two_digit_counter() {
        let mut labels = HashMap::new();
        assert_eq!(derive_category_id("內部專案代號", &labels), "CUSTOM_01");
        labels.insert("CUSTOM_01".to_string(), "內部專案代號".to_string());
        // A different CJK label takes the next free counter.
        assert_eq!(derive_category_id("客戶代碼", &labels), "CUSTOM_02");
        // The same label is idempotent — it keeps its id.
        assert_eq!(derive_category_id("內部專案代號", &labels), "CUSTOM_01");
    }

    #[test]
    fn slug_collision_with_a_different_label_gains_a_suffix() {
        let mut labels = HashMap::new();
        labels.insert("CUSTOM_CODE".to_string(), "Code (finance)".to_string());
        let id = derive_category_id("code", &labels);
        assert_eq!(id, "CUSTOM_CODE_2");
        assert!(is_valid_category(&id));
    }

    #[test]
    fn mixed_label_keeps_only_ascii_alphanumerics() {
        let labels = HashMap::new();
        assert_eq!(derive_category_id("員工 ID", &labels), "CUSTOM_ID");
    }

    #[test]
    fn every_derived_id_is_a_legal_token_category() {
        let labels = HashMap::new();
        for label in [
            "Employee ID",
            "內部專案代號",
            "---",
            "a",
            "MIXED 中文 Label 42",
            "a very very very very long label indeed",
        ] {
            let id = derive_category_id(label, &labels);
            assert!(is_valid_category(&id), "{label} -> {id}");
            // The real gate: the token layer must accept it.
            assert!(
                crate::token::Token::new(&id, &"0".repeat(32)).is_ok(),
                "{label} -> {id} rejected by token validation"
            );
        }
    }
}
