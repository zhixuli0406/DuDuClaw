//! WP16 — channel-side approval buttons (decide in the IM, not by typing).
//!
//! The four button-capable platforms (Telegram / Slack / Discord / LINE) already
//! deliver click events; what was missing is (a) a compact, tamper-evident
//! `action_id` codec, (b) fail-closed authorization so only the bound approver
//! can decide (and a forwarded button can't be used by someone else), and (c) a
//! one-time nonce against replay. This module owns those deterministic pieces so
//! they are unit-tested independently of the per-platform plumbing.
//!
//! `action_id` wire format: `approval:<approval_id>:<approve|deny>:<nonce>`.
//! Telegram caps callback_data at 64 bytes; a UUID approval id + 8-hex nonce
//! fits (`approval:` 9 + 36 + `:approve:` 9 + 8 = 62).

/// A decision carried by an approval button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonDecision {
    Approve,
    Deny,
}

impl ButtonDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ButtonDecision::Approve => "approve",
            ButtonDecision::Deny => "deny",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(ButtonDecision::Approve),
            "deny" => Some(ButtonDecision::Deny),
            _ => None,
        }
    }
}

/// A decoded approval button action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAction {
    pub approval_id: String,
    pub decision: ButtonDecision,
    pub nonce: String,
}

/// Generate a short (8 hex char) one-time nonce for an approval button.
pub fn generate_nonce() -> String {
    // First 8 hex chars of a v4 UUID — enough entropy for a short-lived,
    // single-use token, and keeps callback_data under Telegram's 64-byte cap.
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Encode an `action_id` for a button. `nonce` should come from
/// [`generate_nonce`] and be persisted alongside the approval so the click can
/// be validated + marked used (one-time).
pub fn encode_action_id(approval_id: &str, decision: ButtonDecision, nonce: &str) -> String {
    format!("approval:{approval_id}:{}:{nonce}", decision.as_str())
}

/// Decode an `action_id` produced by [`encode_action_id`]. Returns `None` for
/// anything that isn't a well-formed approval action (fail-closed: an
/// unrecognised button is ignored, never treated as a decision).
pub fn decode_action_id(action_id: &str) -> Option<ApprovalAction> {
    let rest = action_id.strip_prefix("approval:")?;
    // Split from the RIGHT so an approval id containing ':' (it won't, UUIDs
    // don't, but be defensive) doesn't corrupt the nonce/decision parse.
    let (id_and_decision, nonce) = rest.rsplit_once(':')?;
    let (approval_id, decision_str) = id_and_decision.rsplit_once(':')?;
    if approval_id.is_empty() || nonce.is_empty() {
        return None;
    }
    let decision = ButtonDecision::parse(decision_str)?;
    Some(ApprovalAction {
        approval_id: approval_id.to_string(),
        decision,
        nonce: nonce.to_string(),
    })
}

/// Fail-closed authorization: the channel user who pressed the button must be
/// the EXACT bound approver (coding convention #2 — exact equality, never
/// contains/prefix). This is what stops a forwarded button message from being
/// actioned by whoever received the forward: their `presser_user_id` won't match.
pub fn is_authorized_presser(presser_user_id: &str, bound_approver_user_id: &str) -> bool {
    !presser_user_id.is_empty()
        && !bound_approver_user_id.is_empty()
        && presser_user_id == bound_approver_user_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let enc = encode_action_id(id, ButtonDecision::Approve, "a1b2c3d4");
        assert_eq!(enc, "approval:550e8400-e29b-41d4-a716-446655440000:approve:a1b2c3d4");
        assert!(enc.len() <= 64, "must fit Telegram callback_data cap");
        let dec = decode_action_id(&enc).unwrap();
        assert_eq!(dec.approval_id, id);
        assert_eq!(dec.decision, ButtonDecision::Approve);
        assert_eq!(dec.nonce, "a1b2c3d4");
    }

    #[test]
    fn decode_rejects_garbage_fail_closed() {
        assert!(decode_action_id("").is_none());
        assert!(decode_action_id("hello").is_none());
        assert!(decode_action_id("approval:id:maybe:n").is_none()); // bad decision
        assert!(decode_action_id("approval:id:approve:").is_none()); // empty nonce
        assert!(decode_action_id("approval::approve:n").is_none()); // empty id
        assert!(decode_action_id("other:id:approve:n").is_none()); // wrong prefix
    }

    #[test]
    fn authorization_is_exact_match() {
        assert!(is_authorized_presser("u-123", "u-123"));
        // Superstring / prefix must NOT pass (forwarded-message defense).
        assert!(!is_authorized_presser("u-1234", "u-123"));
        assert!(!is_authorized_presser("u-12", "u-123"));
        // Empty either side is unauthorized (fail-closed).
        assert!(!is_authorized_presser("", "u-123"));
        assert!(!is_authorized_presser("u-123", ""));
    }

    #[test]
    fn nonce_is_short_and_hex() {
        let n = generate_nonce();
        assert_eq!(n.len(), 8);
        assert!(n.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
