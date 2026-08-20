//! Webhook channel identifiers and their forwarded-header allowlists.
//!
//! Only `line` is wired up today. Adding a new channel is a single new
//! match arm — the routing (`/v1/hook/{channel}/{device_id}`), body
//! handling, and relay behavior (opaque bytes, no signature verification)
//! stay identical for every channel.
//!
//! The type itself now lives in `duduclaw_core::relay_protocol` (WP-E2) so
//! the box-side relay client shares exactly one definition instead of
//! hand-copying it — see that module's doc for the full rationale. This
//! file re-exports it under its historical path so every existing
//! `crate::channel::Channel` reference in this crate keeps compiling
//! unchanged.
pub use duduclaw_core::relay_protocol::Channel;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_channel() {
        assert_eq!(Channel::parse("line"), Some(Channel::Line));
    }

    #[test]
    fn rejects_unknown_channel() {
        assert_eq!(Channel::parse("whatsapp"), None);
        assert_eq!(Channel::parse(""), None);
        assert_eq!(Channel::parse("LINE"), None); // case-sensitive, no surprise matches
    }

    #[test]
    fn line_forwards_its_signature_header() {
        assert_eq!(Channel::Line.forwarded_headers(), &["x-line-signature"]);
    }
}
