//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── Run inspector helpers (G12) ────────────────────────────────────────────
//
// Pure functions (no I/O beyond the jsonl loader) so the run derivation is
// unit-testable. A run = one `user` turn → the next `assistant` turn of the
// same session; tool receipts are matched by agent + time window from the MCP
// audit trail. No new store is invented — everything derives from what the
// gateway already persists.

/// Default / max page size for `runs.list`.
pub(crate) const RUNS_LIST_DEFAULT_LIMIT: usize = 50;
pub(crate) const RUNS_LIST_MAX_LIMIT: usize = 200;

/// WP3 — `chat.sessions.list` page size (default / hard cap).
pub(crate) const CHAT_SESSIONS_LIST_DEFAULT_LIMIT: usize = 50;
pub(crate) const CHAT_SESSIONS_LIST_MAX_LIMIT: usize = 200;
/// WP3 — `chat.sessions.history` turn count (default / hard cap). Sessions
/// auto-compress at 50k tokens, so live turn counts stay bounded well below
/// this; the cap only guards against a pathological pre-compression backlog.
pub(crate) const CHAT_HISTORY_DEFAULT_LIMIT: usize = 500;
pub(crate) const CHAT_HISTORY_MAX_LIMIT: usize = 2000;
/// WP3 — per-message character cap for history payloads (CJK-safe; normal
/// turns pass through whole, only pathological single messages are bounded).
pub(crate) const CHAT_HISTORY_MSG_MAX_CHARS: usize = 20_000;

/// O-4→O-3 resume wiring: reconstruct a `chat.sessions.history` row's display
/// text and (for assistant turns) inline artifact from the raw session-store
/// content.
///
/// `SessionManager::append_message("assistant", …)` is called from inside
/// `channel_reply::build_reply_with_session_inner`, which runs BEFORE the
/// live-path `strip_operator_pending_marker` call in its two guarded
/// wrappers (`build_guarded_reply_for_agent` /
/// `build_guarded_reply_with_session` — see
/// `channel_reply::strip_operator_pending_marker`'s doc comment; these
/// replaced the former `*_with_artifact` spellings). So the session store
/// holds the RAW text, marker and all; only
/// the live socket frame ever saw the stripped half. Replaying the same
/// strip+map here on read is what makes a resumed conversation reconstruct
/// the same confirm-action card the live frame carried, instead of leaking
/// the bare `<system_operator_pending>…</system_operator_pending>` tag into
/// the transcript.
///
/// Order matters: strip the sender-prefix and operator-pending tag BEFORE
/// truncating. Truncating first could cut a marker in half, and
/// `strip_system_operator_pending_tag` fails open (returns the input
/// unchanged) when it can't find a matching close tag — that would leave a
/// truncated, still-visible marker fragment in the displayed text. Stripping
/// first guarantees the truncation only ever sees ordinary human text.
///
/// `os_operator::strip_system_operator_pending_tag` is a fail-open no-op on
/// ordinary text (the overwhelming majority of rows never went through O-4),
/// and `os_operator::marker_to_artifact` fails closed to `None` on anything
/// it can't map — a malformed or unmappable marker degrades to plain text,
/// never a broken card. Only assistant turns are ever eligible: the marker is
/// only ever produced in assistant output, so user/system rows pass through
/// with no artifact. Pure — exported for tests.
pub(crate) fn chat_history_row_content_and_artifact(role: &str, raw_content: &str) -> (String, Option<Value>) {
    let prefix_stripped = crate::channel_reply::strip_sender_prefix(raw_content);
    let (body, artifact) = if role == "assistant" {
        let (stripped, marker) =
            crate::os_operator::strip_system_operator_pending_tag(prefix_stripped);
        let artifact = marker
            .as_ref()
            .and_then(crate::os_operator::marker_to_artifact);
        (stripped, artifact)
    } else {
        (prefix_stripped.to_string(), None)
    };
    // CJK-safe cap — long single turns are bounded so the payload stays sane;
    // normal messages pass through whole.
    let content = duduclaw_core::truncate_chars(&body, CHAT_HISTORY_MSG_MAX_CHARS);
    (content, artifact)
}
