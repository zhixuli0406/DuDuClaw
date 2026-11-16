//! Closed startup diagnostics; never carry upstream messages or credential data.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    SpawnScopeUnavailable,
    ServerMissing,
    CommandMissing,
    IdentityRejected,
    EnvironmentRejected,
    RedactionUnavailable,
    UpstreamSpawnFailed,
    UpstreamIoUnavailable,
    PolicyDenied,
    ProtocolError,
    Timeout,
    ConnectionFailed,
}

/// Treat labels as untrusted claims, never as authority or free-form log text.
pub fn safe_label(value: &str) -> String {
    value.chars().take(128).map(|c| {
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }
    }).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartStatus {
    pub ok: bool,
    pub reason_code: Option<ReasonCode>,
    pub time: String,
    #[serde(default)]
    pub tool_count: Option<usize>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_are_bounded_and_cannot_inject_log_lines() {
        let label = super::safe_label(&format!("員工\n\r\t{}", "🦀".repeat(200)));
        assert_eq!(label.chars().count(), 128);
        assert!(!label.chars().any(char::is_control));
        assert!(label.starts_with("員工___"));
    }
}
