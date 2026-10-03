//! LLM output contract primitives.
//!
//! An LLM reply is untrusted input. These primitives turn "the model said so"
//! into checks a program can enforce, and they are shared by every consumer
//! (secaudit, the red-team runner, and later the goal loop's judges).
//!
//! **Rule: callers never repair LLM output. A contract violation discards the
//! reply** (retry, mark the unit incomplete, or escalate — never patch the
//! text, slice out "the JSON part", or guess a missing field).
//!
//! - [`strict_json`] — the whole reply, after trimming and removing at most
//!   one outer ```` ```json ```` fence, must be exactly one JSON value of the
//!   expected type, under a size limit. Prose before or after it, a second
//!   value, or a wrong shape voids the reply.
//! - [`safe_path`] — [`safe_path::SafeRepoPath`] accepts only a
//!   repository-relative POSIX path with no absolute/drive/UNC prefix, no
//!   `.`/`..`/empty component, no invisible or control characters and no
//!   Windows device names, so joining it to a root can never escape the root.
//! - [`fingerprint`] — [`fingerprint::Fingerprint`] is a stable, readable
//!   root-cause id derived only from semantic fields (never line, snippet,
//!   severity or verdict), so the same issue matches across runs.
//!   [`fingerprint::canonical_id`] builds a lossless id from a tuple of refs
//!   (NFC + RFC 3986 percent-encoding, joined by `::`).
//! - [`coverage`] — a closed state table for coverage units: each status
//!   fixes which of owner / reviewed paths / checks / result fingerprints /
//!   unresolved reasons must be present, and the reviewed paths must equal
//!   what the checks actually reviewed. Also the run-level `RunStatus` and
//!   `IncompleteReason` enums.
//! - [`severity`] — ordered severity levels, the rule that overall severity
//!   cannot exceed demonstrated impact, and prompt anchors (zh-TW and en) that
//!   calibrate what each level means.
//! - [`visible_text`] — "has something a human can see" and bounded trimmed
//!   text predicates, rejecting strings made only of whitespace, control,
//!   format or default-ignorable code points.
//!
//! Provenance: the mechanics are ported from Cloudflare's security-audit-skill
//! (MIT licence; `validate-findings.cjs`, `validate-coverage-ledger.cjs` and
//! `SKILL.md`). Character-class and state-table rules mirror those validators;
//! see each module for the exact correspondence.

pub mod coverage;
pub mod fingerprint;
pub mod safe_path;
pub mod severity;
pub mod strict_json;
pub mod visible_text;
