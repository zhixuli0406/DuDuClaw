//! Expiring, session-bound navigation context. A valid signature grants no
//! permission; callers must independently re-read the viewer's live access.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::path::Path;

pub(crate) mod delivery;

const MAX_AGE: u64 = 30 * 60;
const MAX_TOKEN: usize = 1024;
const UNAVAILABLE: &str = "watch link unavailable";
const EXPIRED: &str = "watch link expired";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claims {
    version: u8,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) issued_at: i64,
    pub(crate) expires_at: i64,
    nonce: String,
}

pub(crate) fn issue_at(
    home: &Path,
    agent: &str,
    session: &str,
    now: i64,
    remaining_secs: u64,
) -> Result<String, String> {
    if !duduclaw_core::is_valid_agent_id(agent)
        || !valid_session(session)
        || now < 0
        || remaining_secs == 0
    {
        return Err(UNAVAILABLE.into());
    }
    let expires_at = now
        .checked_add(remaining_secs.min(MAX_AGE) as i64)
        .ok_or(UNAVAILABLE)?;
    let claims = Claims {
        version: 1,
        agent_id: agent.into(),
        session_id: session.into(),
        issued_at: now,
        expires_at,
        nonce: uuid::Uuid::new_v4().as_simple().to_string(),
    };
    let payload = serde_json::to_vec(&claims).map_err(|_| UNAVAILABLE)?;
    let secret = crate::config_crypto::watch_link_key(home)?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_ref());
    let tag = ring::hmac::sign(&key, &payload);
    let token = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(tag.as_ref())
    );
    if token.len() > MAX_TOKEN {
        return Err(UNAVAILABLE.into());
    }
    Ok(token)
}

pub(crate) fn verify_at(home: &Path, token: &str, now: i64) -> Result<Claims, String> {
    if token.is_empty() || token.len() > MAX_TOKEN || now < 0 {
        return Err(EXPIRED.into());
    }
    let (body, signature) = token.split_once('.').ok_or(EXPIRED)?;
    let payload = URL_SAFE_NO_PAD.decode(body).map_err(|_| EXPIRED)?;
    let tag = URL_SAFE_NO_PAD.decode(signature).map_err(|_| EXPIRED)?;
    if payload.len() > 512
        || tag.len() != 32
        || URL_SAFE_NO_PAD.encode(&payload) != body
        || URL_SAFE_NO_PAD.encode(&tag) != signature
    {
        return Err(EXPIRED.into());
    }
    let secret = crate::config_crypto::watch_link_key(home).map_err(|_| EXPIRED)?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_ref());
    ring::hmac::verify(&key, &payload, &tag).map_err(|_| EXPIRED)?;
    let claims: Claims = serde_json::from_slice(&payload).map_err(|_| EXPIRED)?;
    if claims.version != 1
        || !duduclaw_core::is_valid_agent_id(&claims.agent_id)
        || !valid_session(&claims.session_id)
        || claims.issued_at < 0
        || claims.issued_at > now
        || claims.expires_at <= now
        || claims
            .expires_at
            .checked_sub(claims.issued_at)
            .is_none_or(|age| age <= 0 || age > MAX_AGE as i64)
        || !lower_hex_32(&claims.nonce)
    {
        return Err(EXPIRED.into());
    }
    Ok(claims)
}

fn lower_hex_32(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_session(value: &str) -> bool {
    value.strip_prefix("cu-").is_some_and(lower_hex_32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoticeRoute {
    WebChat,
    TextOnly,
    ExternalBase(String),
}

/// Delivery policy only. Its caller must separately prove a live attended
/// runtime call and obtain the exact transport from the host turn registry.
pub(crate) fn notice_route(
    channel: &str,
    mapped: bool,
    dashboard_base: Option<&str>,
) -> Option<NoticeRoute> {
    if channel == "webchat" {
        return mapped.then_some(NoticeRoute::WebChat);
    }
    if !matches!(channel, "telegram" | "line" | "discord" | "slack") {
        return None;
    }
    let base = mapped.then_some(dashboard_base).flatten()
        .and_then(|base| url::Url::parse(base).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https")
            && url.username().is_empty() && url.password().is_none()
            && url.query().is_none() && url.fragment().is_none())
        .filter(|url| match url.host() {
            Some(url::Host::Ipv4(ip)) => !ip.is_loopback() && !ip.is_unspecified(),
            Some(url::Host::Ipv6(ip)) => !ip.is_loopback() && !ip.is_unspecified()
                && ip.to_ipv4().is_none_or(|v4| !v4.is_loopback() && !v4.is_unspecified()),
            Some(url::Host::Domain(host)) => {
                let host = host.trim_end_matches('.').to_ascii_lowercase();
                host != "localhost" && !host.ends_with(".localhost")
            }
            None => false,
        });
    Some(match base {
        Some(base) => NoticeRoute::ExternalBase(base.as_str().trim_end_matches('/').into()),
        None => NoticeRoute::TextOnly,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "cu-0123456789abcdef0123456789abcdef";
    fn home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".keyfile"), [42; 32]).unwrap();
        duduclaw_core::platform::set_owner_only(&home.path().join(".keyfile")).unwrap();
        home
    }

    #[test]
    fn navigation_context_roundtrips_but_never_outlives_thirty_minutes() {
        let home = home();
        let token = issue_at(home.path(), "alice", SESSION, 100, 4000).unwrap();
        let claims = verify_at(home.path(), &token, 101).unwrap();
        assert_eq!(claims.agent_id, "alice");
        assert_eq!(claims.session_id, SESSION);
        assert_eq!(claims.expires_at, 1900);
        assert!(verify_at(home.path(), &token, 1900).is_err());
        assert!(verify_at(home.path(), &token, 99).is_err());
        let short = issue_at(home.path(), "alice", SESSION, 100, 5).unwrap();
        assert!(verify_at(home.path(), &short, 104).is_ok());
        assert!(verify_at(home.path(), &short, 105).is_err());
    }

    #[test]
    fn tampering_key_rotation_and_another_gateway_home_invalidate_context() {
        let home = home();
        let token = issue_at(home.path(), "alice", SESSION, 100, 60).unwrap();
        let mut tampered = token.clone().into_bytes();
        tampered[8] = if tampered[8] == b'A' { b'B' } else { b'A' };
        assert!(verify_at(home.path(), std::str::from_utf8(&tampered).unwrap(), 101).is_err());
        let other = self::home(); // Same master bytes, different gateway namespace.
        assert!(verify_at(other.path(), &token, 101).is_err());
        std::fs::write(home.path().join(".keyfile"), [43; 32]).unwrap();
        assert!(verify_at(home.path(), &token, 101).is_err());
    }

    #[test]
    fn repeated_links_have_distinct_context_nonces_without_credentials() {
        let home = home();
        let a = issue_at(home.path(), "alice", SESSION, 100, 60).unwrap();
        let b = issue_at(home.path(), "alice", SESSION, 100, 60).unwrap();
        assert_ne!(a, b);
        assert_eq!(verify_at(home.path(), &a, 100).unwrap().nonce.len(), 32);
        assert!(!a.contains("Bearer"));
    }

    #[test]
    fn malformed_or_oversized_context_and_bad_targets_are_refused() {
        let home = home();
        for token in ["", "a.b.c", "../", "jwt", "🔒"] {
            assert!(verify_at(home.path(), token, 100).is_err());
        }
        assert!(verify_at(home.path(), &"a".repeat(2049), 100).is_err());
        for agent in ["", "../alice", "alice?jwt=x"] {
            assert!(issue_at(home.path(), agent, SESSION, 100, 60).is_err());
        }
        for session in ["", "../", "cu-other", "cu-0123456789abcdef0123456789abcdeg"] {
            assert!(issue_at(home.path(), "alice", session, 100, 60).is_err());
        }
        assert!(issue_at(home.path(), "alice", SESSION, 100, 0).is_err());
    }

    #[test]
    fn a_missing_or_malformed_machine_key_never_creates_a_replacement() {
        let home = tempfile::tempdir().unwrap();
        assert!(issue_at(home.path(), "alice", SESSION, 100, 60).is_err());
        assert!(!home.path().join(".keyfile").exists());
        std::fs::write(home.path().join(".keyfile"), [7; 3]).unwrap();
        assert!(issue_at(home.path(), "alice", SESSION, 100, 60).is_err());
        assert_eq!(
            std::fs::read(home.path().join(".keyfile")).unwrap().len(),
            3
        );
    }

    #[test]
    fn external_notice_never_contains_an_unusable_loopback_or_unsafe_base() {
        for base in [
            "http://localhost:18899",
            "http://localhost.:18899",
            "http://127.1:18899",
            "http://[::1]:18899",
            "http://[::ffff:127.0.0.1]:18899",
            "http://[::ffff:7f00:1]:18899",
            "http://[::ffff:0.0.0.0]:18899",
            "http://[::ffff:0:0]:18899",
            "http://0.0.0.0:18899",
            "http://[::]:18899",
            "file:///private/home",
            "javascript:alert(1)",
            "https://user:secret@dashboard.example",
            "https://dashboard.example?token=secret",
            "https://dashboard.example/#secret",
        ] {
            assert_eq!(
                notice_route("telegram", true, Some(base)),
                Some(NoticeRoute::TextOnly),
                "{base}"
            );
        }
        assert_eq!(notice_route("slack", true, None), Some(NoticeRoute::TextOnly));
    }

    #[test]
    fn only_recognized_internal_conversations_receive_navigation_context() {
        assert_eq!(notice_route("webchat", true, None), Some(NoticeRoute::WebChat));
        for channel in ["telegram", "line", "discord", "slack"] {
            assert_eq!(
                notice_route(channel, true, Some("https://dashboard.example/app/")),
                Some(NoticeRoute::ExternalBase("https://dashboard.example/app".into()))
            );
            assert_eq!(
                notice_route(channel, false, Some("https://dashboard.example")),
                Some(NoticeRoute::TextOnly)
            );
        }
        for channel in ["odoolivechat", "widget", "public_trigger", "googlechat", "", "unknown"] {
            assert_eq!(notice_route(channel, true, Some("https://dashboard.example")), None);
        }
        assert_eq!(notice_route("webchat", false, None), None);
    }
}
