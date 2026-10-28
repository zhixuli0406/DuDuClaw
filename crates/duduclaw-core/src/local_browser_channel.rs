//! Wire authentication shared by the gateway and its native local companion.
//! Dashboard JWTs and employee internal keys are not credentials on this lane.
mod protocol;
pub use protocol::*;

/// Shared operator/bootstrap guard. Presence, even an empty value, counts.
pub const AI_SESSION_ENV_VARS: &[&str] = &[
    crate::ENV_AGENT_ID,
    crate::ENV_AGENT_TOKEN,
    crate::ENV_TRUST_TURN_ID,
    crate::ENV_TRUST_SESSION_ID,
    crate::ENV_TURN_USER_MESSAGE_SEQ,
    crate::ENV_TURN_USER_MESSAGE_AT,
    crate::ENV_DISPATCH_SESSION,
    crate::ENV_DISPATCH_RUN_ID,
    crate::ENV_UPSTREAM_UNKNOWN,
    crate::ENV_DELEGATION_SENDER,
    crate::ENV_DELEGATION_ORIGIN,
    crate::ENV_DELEGATION_DEPTH,
    crate::ENV_HOP_DEPTH,
    crate::ENV_REPLY_CHANNEL,
    crate::ENV_REPLY_SENDER,
    crate::ENV_TASK_ID,
    crate::ENV_LANE,
    crate::ENV_ON_BEHALF_OF,
    "DUDUCLAW_PRINCIPAL_CONTEXT",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_DATA_FILE_GUARD",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_and_replies_are_bound_to_direction_nonce_clock_and_exact_body() {
        let key = [9; 32];
        let nonce = "a1234567890123456789012345678901";
        let body = b"synthetic-heartbeat";
        let signature = sign(&key, false, 100, nonce, body).unwrap();
        assert!(verify(&key, false, 100, nonce, body, &signature, 100));
        assert!(!verify(&key, true, 100, nonce, body, &signature, 100));
        assert!(!verify(
            &key, false, 100, nonce, b"approve", &signature, 100
        ));
        assert!(!verify(&key, false, 100, nonce, body, &signature, 131));
        assert!(!verify(&[8; 32], false, 100, nonce, body, &signature, 100));
        assert!(!verify(
            &key,
            false,
            100,
            "b1234567890123456789012345678901",
            body,
            &signature,
            100
        ));
        assert!(!verify(&key, false, 100, nonce, body, "JWT", 100));
    }
    #[test]
    fn keychain_namespace_is_home_specific() {
        assert_ne!(
            account(std::path::Path::new("/isolated-a")),
            account(std::path::Path::new("/isolated-b"))
        );
        assert_ne!(SERVICE, "duduclaw");
    }
}
