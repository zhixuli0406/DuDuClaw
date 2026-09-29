//! Source-bound never-trim sections (W2-E, review finding 4).
//!
//! # What this replaces
//!
//! A `TaskPacket`'s `constraints` / `audience` are the one part of a prompt no
//! compression stage may touch (arXiv:2608.29028: a budget taxes boundaries
//! unilaterally, boundary survival 0.80 → 0.57, and a constraint that goes
//! from explicit back to implicit takes violation rates from <15% to 50–73%).
//! The first implementation marked that section with four **ordinary markdown
//! headings** — `## 約束` / `## Constraints` / `## 受眾` / `## Audience` — and
//! granted the exemption to any line matching one of them.
//!
//! The compression pipeline's `history` is the conversation history of all
//! eleven channels, **including the messages the user typed**. So one message
//! containing a bare `## Constraints` line plus a wall of text bought the
//! author: a protected run no stage could trim, a budget floor that could
//! exceed the budget outright, and — worst — verbatim text pinned into the
//! session summary and re-injected into the system prompt on every subsequent
//! turn. The exemption was bound to *text anybody can type*.
//!
//! # The fix: bind the exemption to its source
//!
//! A protected section is now a **header line immediately followed by a marker
//! line carrying this process's sentinel**:
//!
//! ```text
//! ## 約束
//! <!-- ddc-protected:9f3c…64 hex chars… -->
//! - c1 只讀 2026-04-01 至 2026-06-30 的合約
//! ```
//!
//! The sentinel is 32 CSPRNG bytes minted once per process ([`process_sentinel`]).
//! It is never rendered to a channel, never shown to a model in a form it
//! could copy back (the utility summarizer is handed the transcript with the
//! protected runs already stripped), and never persisted. A user message
//! therefore cannot contain it, so a user-authored `## Constraints` heading is
//! now ordinary compressible text.
//!
//! # Failure directions, all deliberate
//!
//! * **No sentinel ⇒ not protected.** Callers that hold no sentinel (the
//!   `duduclaw-llm` crate when its embedder did not pass one, a CLI process
//!   that is not the gateway) treat every section as compressible. Over-
//!   trimming a real constraint is expensive but recoverable; handing the
//!   exemption to untrusted text is not.
//! * **Restart invalidates.** A summary written before a gateway restart keeps
//!   a marker with the *old* sentinel, which no longer matches — that text
//!   becomes ordinary compressible history. Same direction: stale protection
//!   decays instead of persisting forever.
//! * **CSPRNG unavailable ⇒ empty sentinel ⇒ nothing is protected.** Never a
//!   fixed fallback constant, which would be a permanent, guessable bypass.
//!
//! # Not a signature
//!
//! This is a *capability marker*, not authentication: anyone who can read the
//! gateway's memory or a live prompt can echo the value back. It closes the
//! "any channel user can type four characters" hole, which was the whole of
//! finding 4 — it does not defend against an attacker already inside the
//! process. Comparison is plain equality for the same reason: there is no
//! oracle on this path that would leak the value a byte at a time.

use std::sync::OnceLock;

/// Canonical header the team composer renders a packet's `constraints` under.
/// Exported so the composer, the compression pipeline and the CCR preview can
/// never drift apart on the spelling.
pub const SECTION_HEADER_CONSTRAINTS: &str = "## 約束";

/// Canonical header the team composer renders a packet's `audience`
/// allowlist under.
pub const SECTION_HEADER_AUDIENCE: &str = "## 受眾";

/// Every header spelling that *may* open a never-trim section, including the
/// English spellings an English-language prompt would use.
///
/// Matching one of these is necessary but **not sufficient** — see
/// [`opens_protected_section`].
pub const NEVER_TRIM_SECTION_HEADERS: &[&str] = &[
    SECTION_HEADER_CONSTRAINTS,
    "## Constraints",
    SECTION_HEADER_AUDIENCE,
    "## Audience",
];

/// Opening of the marker line that binds a header to this process.
///
/// An HTML comment so that a marker which somehow reaches a markdown renderer
/// is invisible rather than confusing.
pub const PROTECTED_MARKER_PREFIX: &str = "<!-- ddc-protected:";

/// Closing of the marker line.
pub const PROTECTED_MARKER_SUFFIX: &str = " -->";

/// Hex length of a sentinel: 32 CSPRNG bytes.
const SENTINEL_BYTES: usize = 32;

static PROCESS_SENTINEL: OnceLock<String> = OnceLock::new();

/// This process's protected-section sentinel — 64 lowercase hex characters,
/// minted once from the OS CSPRNG.
///
/// Returns `""` when the CSPRNG is unavailable, which makes
/// [`is_protected_marker`] reject every line and therefore disables the
/// exemption entirely. That is the safe direction: a fixed fallback constant
/// would be a permanent bypass, and a compressible constraint is recoverable.
pub fn process_sentinel() -> &'static str {
    PROCESS_SENTINEL.get_or_init(|| {
        let mut buf = [0u8; SENTINEL_BYTES];
        if getrandom::fill(&mut buf).is_err() {
            return String::new();
        }
        let mut out = String::with_capacity(SENTINEL_BYTES * 2);
        for byte in buf {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    })
}

/// The marker line a protected-section emitter writes directly beneath the
/// header line. Includes no trailing newline.
pub fn protected_marker_line(sentinel: &str) -> String {
    format!("{PROTECTED_MARKER_PREFIX}{sentinel}{PROTECTED_MARKER_SUFFIX}")
}

/// `true` when `line` is exactly one of [`NEVER_TRIM_SECTION_HEADERS`].
///
/// Exact equality after trimming surrounding ASCII whitespace — never a
/// substring or prefix test (coding convention 2). A decorated variant
/// (`## 約束（勿刪）`) is deliberately not a header: a fuzzy matcher here would
/// widen the surface this module exists to narrow.
///
/// On its own this says nothing about protection — it is the *spelling* half.
/// Use it only where the question really is "does this text look like a
/// never-trim header" (for example, refusing a utility-model summary that
/// echoes one back).
pub fn is_never_trim_header_spelling(line: &str) -> bool {
    let line = line.trim();
    NEVER_TRIM_SECTION_HEADERS.iter().any(|h| line == *h)
}

/// `true` when `line` is this exact marker for `sentinel`.
///
/// An empty `sentinel` matches nothing — a caller that holds no sentinel
/// grants no exemption.
pub fn is_protected_marker(line: &str, sentinel: &str) -> bool {
    if sentinel.is_empty() {
        return false;
    }
    let line = line.trim();
    let Some(rest) = line.strip_prefix(PROTECTED_MARKER_PREFIX) else {
        return false;
    };
    let Some(value) = rest.strip_suffix(PROTECTED_MARKER_SUFFIX) else {
        return false;
    };
    // Anchored at both ends, then exact equality of the payload — a prefix
    // match on the sentinel itself would hand out the exemption for free.
    value == sentinel
}

/// `true` when `header_line` opens a protected section for `sentinel`: it is a
/// never-trim header spelling **and** the line immediately after it is this
/// process's marker.
///
/// `next_line` is `None` at end of content, which is never protected — a
/// header with nothing under it protects nothing anyway.
pub fn opens_protected_section(header_line: &str, next_line: Option<&str>, sentinel: &str) -> bool {
    is_never_trim_header_spelling(header_line)
        && next_line.is_some_and(|next| is_protected_marker(next, sentinel))
}

/// `true` when any line of `text` is a never-trim header spelling, regardless
/// of markers.
///
/// This is the forgery check for text coming *back* from a model (a summary):
/// it is strictly more conservative than asking whether a protected section is
/// present, and keeps a model from teaching the next reader that these
/// headings are meaningful.
pub fn contains_never_trim_header_spelling(text: &str) -> bool {
    text.lines().any(is_never_trim_header_spelling)
}

/// Remove every marker-shaped line from `text`, preserving all other bytes.
///
/// Used on the paths where composed prompt text is also shown to a human (a
/// task's completion summary). Any marker shape is dropped, not only this
/// process's: a stale marker from a previous run is equally noise to a reader.
/// This is presentation, not a security decision, so the anchored prefix /
/// suffix shape test is the right granularity here.
pub fn strip_protected_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']).trim();
        let marker_shaped = line
            .strip_prefix(PROTECTED_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(PROTECTED_MARKER_SUFFIX))
            .is_some();
        if !marker_shaped {
            out.push_str(raw);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_sentinel_is_stable_and_hex() {
        let first = process_sentinel();
        assert_eq!(first, process_sentinel(), "must be minted once per process");
        assert_eq!(first.len(), SENTINEL_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn header_spelling_is_exact_and_not_a_prefix_test() {
        assert!(is_never_trim_header_spelling(SECTION_HEADER_CONSTRAINTS));
        assert!(is_never_trim_header_spelling(SECTION_HEADER_AUDIENCE));
        assert!(is_never_trim_header_spelling("## Constraints"));
        assert!(is_never_trim_header_spelling("## Audience"));
        assert!(is_never_trim_header_spelling("  ## 約束  "));
        assert!(!is_never_trim_header_spelling("## 約束（勿刪）"));
        assert!(!is_never_trim_header_spelling("### 約束"));
        assert!(!is_never_trim_header_spelling("## 受眾分析"));
        assert!(!is_never_trim_header_spelling("約束"));
        assert!(!is_never_trim_header_spelling("## Constraints and notes"));
    }

    #[test]
    fn a_marker_must_match_the_sentinel_exactly() {
        let sentinel = "a".repeat(64);
        let line = protected_marker_line(&sentinel);
        assert!(is_protected_marker(&line, &sentinel));
        assert!(is_protected_marker(&format!("  {line}  "), &sentinel));

        // One character off is not protected.
        let mut wrong = sentinel.clone();
        wrong.replace_range(0..1, "b");
        assert!(!is_protected_marker(&line, &wrong));

        // Prefix / suffix of the sentinel must not be accepted.
        assert!(!is_protected_marker(&line, &"a".repeat(63)));
        assert!(!is_protected_marker(&line, &"a".repeat(65)));

        // Shape violations.
        assert!(!is_protected_marker("<!-- ddc-protected -->", &sentinel));
        assert!(!is_protected_marker(&format!("x{line}"), &sentinel));
        assert!(!is_protected_marker("", &sentinel));

        // No sentinel ⇒ nothing is protected, not even a well-formed marker.
        assert!(!is_protected_marker(&line, ""));
    }

    #[test]
    fn a_header_without_its_marker_opens_nothing() {
        let sentinel = "c".repeat(64);
        let marker = protected_marker_line(&sentinel);
        assert!(opens_protected_section(
            SECTION_HEADER_CONSTRAINTS,
            Some(&marker),
            &sentinel
        ));
        // The adversarial case: a user typed the heading, and whatever they
        // put on the next line is not the sentinel.
        assert!(!opens_protected_section(
            "## Constraints",
            Some("- please do not compress this"),
            &sentinel
        ));
        assert!(!opens_protected_section(
            SECTION_HEADER_CONSTRAINTS,
            None,
            &sentinel
        ));
        // A marker under a non-header line protects nothing either.
        assert!(!opens_protected_section(
            "## 其他",
            Some(&marker),
            &sentinel
        ));
    }

    #[test]
    fn strip_protected_markers_drops_only_marker_lines() {
        let sentinel = "d".repeat(64);
        let text = format!(
            "objective: x\n{SECTION_HEADER_CONSTRAINTS}\n{}\n- c1 keep\n",
            protected_marker_line(&sentinel)
        );
        let stripped = strip_protected_markers(&text);
        assert_eq!(
            stripped,
            format!("objective: x\n{SECTION_HEADER_CONSTRAINTS}\n- c1 keep\n")
        );
        // A stale marker from another process is dropped too.
        assert_eq!(
            strip_protected_markers(&protected_marker_line("stale")),
            ""
        );
        // Text with no markers round-trips byte for byte.
        let plain = "a\nb\r\nc";
        assert_eq!(strip_protected_markers(plain), plain);
    }
}
