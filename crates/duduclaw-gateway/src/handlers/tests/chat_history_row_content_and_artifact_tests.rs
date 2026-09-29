//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// `chat_history_row_content_and_artifact` — the pure resume-path strip+map
/// backing `chat.sessions.history`'s task-B artifact reconstruction. Session
/// storage was confirmed (via `channel_reply::build_reply_with_session_inner`)
/// to persist assistant replies BEFORE the live-path
/// `strip_operator_pending_marker` call, so these tests exercise the read-side
/// replay directly rather than standing up a full `ReplyContext` +
/// `SessionManager` harness.
use super::*;

#[test]
fn assistant_marker_is_stripped_and_mapped_to_artifact() {
    let stored = "「重新開機」會變更這台機器的狀態，請先確認：要執行嗎？\n\n\
            <system_operator_pending>{\"tool\":\"os_power\",\"params\":{\"action\":\"restart\"},\
            \"needs_confirm\":true,\"needs_approval\":false}</system_operator_pending>";
    let (content, artifact) = chat_history_row_content_and_artifact("assistant", stored);
    assert!(
        !content.contains("system_operator_pending"),
        "marker tag must not leak into resumed history text: {content}"
    );
    assert!(
        content.contains("重新開機"),
        "human text must survive: {content}"
    );
    let artifact = artifact.expect("os_power/restart must map to a confirm_action artifact");
    assert_eq!(artifact["type"], "confirm_action");
    assert_eq!(artifact["payload"]["action"], "restart");
}

#[test]
fn assistant_text_without_marker_is_unchanged_and_has_no_artifact() {
    let stored = "訂單 #42 已出貨，預計三天內送達。";
    let (content, artifact) = chat_history_row_content_and_artifact("assistant", stored);
    assert_eq!(content, stored);
    assert!(artifact.is_none());
}

#[test]
fn unknown_tool_marker_is_stripped_but_produces_no_artifact() {
    // marker_to_artifact fails closed to None on a tool it doesn't
    // recognise; the tag itself must still never leak into `content`.
    let stored = "任務已排入佇列。\n\n\
            <system_operator_pending>{\"tool\":\"some_future_tool\",\"params\":{},\
            \"needs_confirm\":true,\"needs_approval\":false}</system_operator_pending>";
    let (content, artifact) = chat_history_row_content_and_artifact("assistant", stored);
    assert!(
        !content.contains("system_operator_pending"),
        "got: {content}"
    );
    assert!(artifact.is_none());
}

#[test]
fn non_assistant_role_is_never_scanned_for_a_marker() {
    // The marker is only ever produced in assistant output (render_pending
    // in os_operator.rs); a user/system row is left exactly as stored —
    // whatever literal text a user pasted is their own and is not treated
    // as a system marker.
    let stored = "使用者貼上的文字剛好包含 <system_operator_pending>不是真的標記</system_operator_pending>";
    let (content, artifact) = chat_history_row_content_and_artifact("user", stored);
    assert_eq!(content, stored);
    assert!(artifact.is_none());
}

#[test]
fn strip_runs_before_truncate_so_a_split_marker_never_leaks() {
    // Regression guard for the ordering bug this task's investigation
    // flagged: truncating the raw (marker-included) text first could cut
    // the closing tag off, which makes `strip_system_operator_pending_tag`
    // fail open and return the ORIGINAL (now truncated, still tagged)
    // text unchanged. Stripping first means truncation only ever sees
    // plain human text, so no fragment of the tag can survive.
    let human = "確認要繼續嗎？";
    let marker = "<system_operator_pending>{\"tool\":\"os_power\",\"params\":{\"action\":\"shutdown\"},\
            \"needs_confirm\":true,\"needs_approval\":false}</system_operator_pending>";
    // Pad the human prefix well past the cap so a naive truncate-first
    // implementation would slice through the marker.
    let padded_human = human.repeat(CHAT_HISTORY_MSG_MAX_CHARS);
    let stored = format!("{padded_human}\n\n{marker}");
    let (content, artifact) = chat_history_row_content_and_artifact("assistant", &stored);
    assert!(
        !content.contains("system_operator_pending"),
        "a split marker must never leak a fragment into the truncated text"
    );
    assert!(content.chars().count() <= CHAT_HISTORY_MSG_MAX_CHARS);
    // The marker survives (it's stripped from the *front* text, not
    // discarded by the cap) so the artifact still reconstructs.
    let artifact =
        artifact.expect("marker must still map even though the human text is capped");
    assert_eq!(artifact["payload"]["action"], "shutdown");
}

#[test]
fn sender_prefix_and_marker_both_strip_together() {
    let stored = "[sender_id: alice]\n「回復原廠設定」是不可逆的系統操作，需要人工核准。\n\n\
            <system_operator_pending>{\"tool\":\"os_factory_reset\",\"params\":{},\
            \"needs_confirm\":false,\"needs_approval\":true}</system_operator_pending>";
    let (content, artifact) = chat_history_row_content_and_artifact("assistant", stored);
    assert!(!content.contains("sender_id"), "got: {content}");
    assert!(
        !content.contains("system_operator_pending"),
        "got: {content}"
    );
    let artifact = artifact.expect("os_factory_reset must map to a confirm_action artifact");
    assert_eq!(artifact["payload"]["action"], "factory_reset");
}
