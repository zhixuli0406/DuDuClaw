//! Gap fingerprinting for the goal loop's no-progress guard — **H4**
//! (`commercial/docs/DESIGN-harness-borrowings-2026-08.md` WP-B, borrowed
//! from grok-build's `gap_fingerprint`, `session/goal_tracker.rs:1208`).
//!
//! ## Problem this replaces
//!
//! [`crate::goal_state::StateBlock::hash_input`] folds the LATEST judge
//! rejection reason into the A2 no-progress guard's `state_hash` verbatim
//! (after NFKC-normalize + whitespace-collapse — see `short_hash` /
//! `normalize_for_hash`). That is still, in substance, a byte comparison: a
//! judge that rewords the exact same underlying gap ("missing error
//! handling in `goal_loop.rs:120`" vs. "you forgot to validate at
//! `goal_loop.rs:120`") produces two different hashes, so the no-progress
//! guard never fires even though the agent is provably stuck on the same
//! spot — the two-round oscillation escalation silently loses its signal to
//! cosmetic rewording.
//!
//! [`gap_fingerprint`] extracts the durable part of a rejection reason —
//! `path:line` citations and backtick-quoted key tokens (function/variable
//! names, error identifiers) — and normalizes it so a reworded-but-same gap
//! collapses to an identical fingerprint. `path:line` extraction here
//! answers "no citation" for the ANY case per docs; when a `path:line`
//! citation or key token is present, a scratch/temp path segment (which
//! frequently contains a random UUID/hash and would otherwise make every
//! citation of the same logical file appear unique — grok's own rationale)
//! is normalized to a `<scratch>` placeholder before hashing.
//!
//! ## Fallback contract
//!
//! When NEITHER a `path:line` citation NOR a backtick key token can be
//! extracted, [`gap_fingerprint`] returns `None` and the caller
//! (`goal_state.rs::StateBlock::hash_input`) falls back to the literal
//! (NFKC-normalized) feedback text — i.e. byte-identical to the pre-H4
//! behavior. This keeps the change behavior-compatible for prose-only
//! rejection reasons (the common case for research/analysis goals with no
//! file citations at all).

use std::sync::LazyLock;

use regex::Regex;

/// Matches a `path:line` or `path:line:col` citation. Requires the "path"
/// half to contain a `.` followed by a short alphabetic extension (`.rs`,
/// `.ts`, `.py`, …) so ordinary sentences with a colon (times, ratios,
/// labels) are not mistaken for citations. Deliberately permissive on the
/// path character class (letters/digits/`_`/`.`/`/`/`\`/`-`) — judges cite
/// paths in many shapes (relative, absolute, Windows) and over-matching a
/// token here only ever adds one more entry to the fingerprint set, it
/// never causes a false "no citation" fallback.
static CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([A-Za-z0-9_][A-Za-z0-9_./\\-]*\.[A-Za-z]{1,10}):(\d{1,7})(?::(\d{1,5}))?")
        .expect("CITATION_RE must compile")
});

/// Matches a backtick-quoted key token, e.g. `` `foo_bar()` `` or
/// `` `TypeError` `` — the common way a judge cites a specific
/// function/variable/error identifier in prose feedback.
static KEY_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`\n]{1,80})`").expect("KEY_TOKEN_RE must compile"));

/// Hard cap on how many extracted tokens feed one fingerprint — bounds
/// pathological input (a feedback string with dozens of citations) without
/// affecting realistic judge feedback, which cites at most a handful of
/// spots.
const MAX_FINGERPRINT_TOKENS: usize = 12;

/// Segment names treated as scratch/temp markers regardless of case.
const SCRATCH_SEGMENT_NAMES: &[&str] = &["tmp", "temp", "scratch"];

/// Extract a stagnation "gap fingerprint" from one rejection-reason string:
/// `path:line[:col]` citations and backtick-quoted key tokens, normalized
/// (scratch-path segments → `<scratch>`, lowercased, deduped, sorted) so a
/// judge that rewords the same underlying gap still produces an identical
/// fingerprint. Returns `None` when no citation or key token can be
/// extracted at all — callers MUST fall back to literal-text comparison in
/// that case (see module docs' "Fallback contract").
pub fn gap_fingerprint(feedback: &str) -> Option<String> {
    let tokens = extract_tokens(feedback);
    if tokens.is_empty() {
        return None;
    }

    let mut normalized: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    normalized.sort();
    normalized.dedup();
    Some(normalized.join("\u{1}"))
}

/// WP-4F: the human-facing twin of [`gap_fingerprint`] — the same
/// `path:line` citations and backtick key tokens, but case-preserved and in
/// original occurrence order (only [`gap_fingerprint`] needs the
/// lowercased/sorted form, for stable hash comparison). Used to render a
/// "what's still missing" list (e.g. the goal-loop budget-exhausted
/// escalation note) and, via its length, to rank rounds by how many concrete
/// gaps remain (fewer ⇒ closer to done). Deduplication is case-insensitive
/// (so `goal_loop.rs:120` and `GOAL_LOOP.rs:120` collapse to one entry) but
/// keeps the first-seen casing. Empty when no citation or key token can be
/// extracted — same fallback contract as `gap_fingerprint`.
pub fn gap_tokens(feedback: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in extract_tokens(feedback) {
        if seen.insert(t.to_lowercase()) {
            out.push(t);
        }
    }
    out
}

/// Shared extraction pass behind [`gap_fingerprint`] / [`gap_tokens`]:
/// `path:line[:col]` citations first, then backtick-quoted key tokens,
/// scratch-path segments normalized, capped at [`MAX_FINGERPRINT_TOKENS`].
/// Case-preserved, not deduplicated — callers apply their own
/// normalization/dedup policy on top.
fn extract_tokens(feedback: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();

    for cap in CITATION_RE.captures_iter(feedback) {
        let path = normalize_scratch_path(&cap[1]);
        let line = &cap[2];
        tokens.push(match cap.get(3) {
            Some(col) => format!("{path}:{line}:{}", col.as_str()),
            None => format!("{path}:{line}"),
        });
        if tokens.len() >= MAX_FINGERPRINT_TOKENS {
            return tokens;
        }
    }

    for cap in KEY_TOKEN_RE.captures_iter(feedback) {
        let token = cap[1].trim();
        if token.is_empty() {
            continue;
        }
        tokens.push(normalize_scratch_path(token));
        if tokens.len() >= MAX_FINGERPRINT_TOKENS {
            break;
        }
    }

    tokens
}

/// Replace scratch/temp-looking path segments with a stable `<scratch>`
/// placeholder so two citations of "the same logical scratch file" under
/// different random temp directories collapse to the same fingerprint —
/// otherwise a UUID or content-hash directory name would make the
/// fingerprint unique on every single rejection, defeating the whole
/// purpose (grok's own documented rationale for the same normalization
/// step). Non-scratch segments are passed through unchanged (case folding
/// happens once, later, over the whole fingerprint).
fn normalize_scratch_path(path: &str) -> String {
    path.split(['/', '\\'])
        .map(|seg| {
            if is_scratch_segment(seg) {
                "<scratch>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn is_scratch_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    let lower = seg.to_ascii_lowercase();
    if SCRATCH_SEGMENT_NAMES.contains(&lower.as_str()) {
        return true;
    }
    is_uuid_like(seg) || is_hex_hash_like(seg)
}

/// `8-4-4-4-12` hex UUID shape (the classic random-temp-dir-name pattern).
fn is_uuid_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(parts.iter())
            .all(|(&len, p)| p.len() == len && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A bare hex-looking segment of meaningful length (content-hash temp dir
/// names, e.g. `9f8c1e2a3b4d5e6f`). 8+ hex chars is a deliberately
/// conservative floor — short real words practically never satisfy
/// "all-hex" (most contain a non-hex letter like `g`/`l`/`o`), so this
/// rarely false-positives on genuine path segments.
fn is_hex_hash_like(s: &str) -> bool {
    s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DoD: same gap reworded → same fingerprint ────────────

    #[test]
    fn same_citation_different_wording_same_fingerprint() {
        let a = "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120, please add a check.";
        let b = "You forgot proper error handling at crates/duduclaw-gateway/src/goal_loop.rs:120 — add validation.";
        let fp_a = gap_fingerprint(a).expect("a has a citation");
        let fp_b = gap_fingerprint(b).expect("b has a citation");
        assert_eq!(
            fp_a, fp_b,
            "reworded feedback citing the same path:line must fingerprint identically"
        );
    }

    #[test]
    fn same_key_token_different_wording_same_fingerprint() {
        let a = "The function `parse_state_update` does not validate its input.";
        let b = "Please validate input inside `parse_state_update` before use.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── DoD: different gap → different fingerprint ───────────

    #[test]
    fn different_citation_different_fingerprint() {
        let a = "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120.";
        let b = "Missing error handling in crates/duduclaw-gateway/src/goal_state.rs:42.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn different_line_same_file_different_fingerprint() {
        let a = "See crates/duduclaw-gateway/src/goal_loop.rs:120 for the missing check.";
        let b = "See crates/duduclaw-gateway/src/goal_loop.rs:999 for the missing check.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── DoD: no citation at all → None (caller falls back to string compare) ──

    #[test]
    fn no_citation_returns_none() {
        assert_eq!(
            gap_fingerprint("The summary is too vague, please clarify what you did."),
            None
        );
        assert_eq!(
            gap_fingerprint("驗收未通過,說明太模糊,請補充你實際做了什麼。"),
            None
        );
    }

    #[test]
    fn empty_feedback_returns_none() {
        assert_eq!(gap_fingerprint(""), None);
        assert_eq!(gap_fingerprint("   "), None);
    }

    // ── Scratch/temp path normalization ───────────────────────

    #[test]
    fn scratch_uuid_dirs_collapse_to_same_fingerprint() {
        let a =
            "Output written to /tmp/9f8c1e2a-1111-2222-3333-444455556666/output.log:12 is wrong.";
        let b =
            "Output written to /tmp/aa11bb22-9999-8888-7777-666655554444/output.log:12 is wrong.";
        let fp_a = gap_fingerprint(a).expect("a has a citation");
        let fp_b = gap_fingerprint(b).expect("b has a citation");
        assert_eq!(
            fp_a, fp_b,
            "two different random scratch dirs citing the same logical file:line must collapse"
        );
    }

    #[test]
    fn scratch_hex_hash_dirs_collapse_to_same_fingerprint() {
        let a = "See /scratch/9f8c1e2a3b4d5e6f/report.json:5.";
        let b = "See /scratch/deadbeefcafebabe/report.json:5.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn non_scratch_paths_are_not_collapsed() {
        // Real, meaningful directory names must survive unnormalized —
        // only tmp/temp/scratch/UUID/hex-hash segments get replaced.
        let a = "See crates/duduclaw-gateway/src/goal_loop.rs:120.";
        let b = "See crates/duduclaw-security/src/goal_loop.rs:120.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── Normalization: lowercase / dedupe / sort (order-independent) ──

    #[test]
    fn fingerprint_is_case_insensitive() {
        let a = "See CRATES/DuDuClaw-Gateway/SRC/Goal_Loop.rs:120.";
        let b = "see crates/duduclaw-gateway/src/goal_loop.rs:120.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = "First check goal_loop.rs:10, then check goal_state.rs:20.";
        let b = "First check goal_state.rs:20, then check goal_loop.rs:10.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn duplicate_citations_dedupe() {
        let single = "See goal_loop.rs:120.";
        let doubled = "See goal_loop.rs:120 — really, goal_loop.rs:120 is broken.";
        assert_eq!(gap_fingerprint(single), gap_fingerprint(doubled));
    }

    #[test]
    fn citation_with_column_is_captured() {
        let fp = gap_fingerprint("Error at goal_loop.rs:120:5 — fix the type.").unwrap();
        assert!(fp.contains("120:5"));
    }
}
