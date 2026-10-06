//! Closed error codes for the review / workflow-draft RPCs (F3 item 10).
//!
//! The dashboard maps `error.code` to a sentence in the reader's language;
//! the raw server text never reaches the screen and is only logged here.
//! `error.message` carries a fixed English description of the code, never
//! the internal text (which can name files, tables or SQL errors).
use super::*;

/// Every code these RPCs can return. Adding one means adding a sentence for
/// it in `web/src/components/workflow/workflow-text.ts` and the three
/// language files.
pub(crate) const WORKFLOW_ERROR_CODES: &[(&str, &str)] = &[
    ("permission_denied", "You do not have access to this item."),
    ("identity_unavailable", "Your account could not be checked."),
    ("manager_required", "A manager role is required."),
    ("admin_required", "An admin role is required."),
    ("snapshot_not_latest", "A newer review snapshot exists."),
    (
        "snapshot_truncated",
        "The snapshot does not hold every deliverable.",
    ),
    ("stale", "The material changed or could not be verified."),
    ("fixture_missing", "A test case has no result yet."),
    ("fixture_expired", "A test result has expired."),
    (
        "fixture_mismatch",
        "A test result does not match its expectation.",
    ),
    (
        "activation_exists",
        "This version was already sent for review.",
    ),
    ("approval_pending", "The admin decision is still pending."),
    ("invalid_proposal", "The proposal is not valid."),
    ("source_task_unfinished", "The source task is not finished."),
    ("generic", "The request could not be completed."),
];

/// Map an internal error text to its code. The table is matched on the
/// whole text or on a fixed leading phrase written by this crate.
pub(crate) fn workflow_error_code(raw: &str) -> &'static str {
    let t = raw.trim();
    const EXACT: &[(&str, &str)] = &[
        ("permission denied", "permission_denied"),
        ("draft not found", "permission_denied"),
        ("review snapshot not found", "permission_denied"),
        ("run not found", "permission_denied"),
        ("identity unavailable", "identity_unavailable"),
        ("identity permission denied", "permission_denied"),
        ("manager role required", "manager_required"),
        ("admin role required", "admin_required"),
        ("review snapshot is not the latest", "snapshot_not_latest"),
        ("review snapshot truncated", "snapshot_truncated"),
        ("fixture evidence missing", "fixture_missing"),
        ("fixture evidence expired", "fixture_expired"),
        ("fixture evidence mismatched", "fixture_mismatch"),
        ("activation already exists", "activation_exists"),
        (
            "a completed source task is required",
            "source_task_unfinished",
        ),
        ("typed proposal required", "invalid_proposal"),
        ("invalid typed proposal", "invalid_proposal"),
        (
            "activation requires real human acceptance",
            "approval_pending",
        ),
        ("activation acceptance missing", "approval_pending"),
    ];
    if let Some((_, code)) = EXACT.iter().find(|(text, _)| *text == t) {
        return code;
    }
    const STALE: &[&str] = &[
        "review snapshot stale or unverified",
        "source review snapshot unavailable",
        "source snapshot changed",
        "fixed snapshot hash mismatch",
        "fixed draft hash mismatch",
        "draft material stale or unverified",
        "source material stale or unverified",
        "activation no longer eligible",
        "active workflow authority changed",
    ];
    if STALE.contains(&t) {
        return "stale";
    }
    const PROPOSAL_PREFIXES: &[&str] = &[
        "draft ",
        "fixture safety expectations",
        "all five fixture kinds",
        "invalid fixed ",
        "expired fixture ",
        "injection fixture ",
        "effect template",
        "unused effect authority",
    ];
    if PROPOSAL_PREFIXES.iter().any(|p| t.starts_with(p)) && t.len() <= 160 {
        return "invalid_proposal";
    }
    "generic"
}

/// Rewrite an error response of these RPCs into `{code, message}`. A frame
/// that already carries an object error (another structured refusal) is
/// left as it is.
pub(crate) fn with_workflow_error_code(method: &str, frame: WsFrame) -> WsFrame {
    match frame {
        WsFrame::Response {
            id,
            ok: false,
            payload,
            error: Some(Value::String(raw)),
        } => {
            let code = workflow_error_code(&raw);
            if code == "generic" {
                warn!(method, error = %raw, "workflow RPC failed");
            }
            let message = WORKFLOW_ERROR_CODES
                .iter()
                .find(|(c, _)| *c == code)
                .map(|(_, m)| *m)
                .unwrap_or("The request could not be completed.");
            WsFrame::Response {
                id,
                ok: false,
                payload,
                error: Some(json!({ "code": code, "message": message })),
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codes_are_closed_and_raw_text_never_leaves() {
        for (raw, code) in [
            ("permission denied", "permission_denied"),
            ("draft not found", "permission_denied"),
            ("review snapshot is not the latest", "snapshot_not_latest"),
            ("fixture evidence expired", "fixture_expired"),
            (
                "fixture safety expectations cannot be weakened",
                "invalid_proposal",
            ),
            (
                "invalid immutable evidence row: expected value at line 1",
                "generic",
            ),
            (
                "open /home/u/.duduclaw/workflow.db: disk I/O error",
                "generic",
            ),
        ] {
            assert_eq!(workflow_error_code(raw), code, "{raw}");
            assert!(WORKFLOW_ERROR_CODES.iter().any(|(c, _)| *c == code));
        }
        let frame = with_workflow_error_code(
            "tasks.review_accept",
            WsFrame::error_response("", "open /home/u/x.db: locked"),
        );
        let WsFrame::Response { error: Some(e), .. } = frame else {
            panic!()
        };
        assert_eq!(e["code"], "generic");
        assert!(!e.to_string().contains("/home/u"));
    }
}
