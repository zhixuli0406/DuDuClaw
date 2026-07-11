//! Email channel — inbound RFC822 parsing + async SMTP send.
//!
//! **Scope & honesty.** The SMTP *send* path here is fully implemented and
//! loopback-verifiable (`send_email` / `build_message`; see the `#[ignore]`
//! `smtp_send_loopback` test run against a local `aiosmtpd`). Inbound-email
//! *parsing* (`parse_inbound`) is pure and unit-tested. The IMAP *poll
//! transport* that would feed `parse_inbound` from a live mailbox is **not**
//! wired here — it needs a real IMAP account to verify, so it is intentionally
//! left as the documented PENDING-LIVE remainder (see `docs/` / the channel
//! roadmap) rather than shipped as untestable code.
//!
//! Email doubles as a fail-safe alert sink: when every chat channel is down, an
//! agent can still reach the operator over SMTP.

use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};

/// TLS mode for the SMTP submission connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Plaintext — loopback / testing only.
    None,
    /// STARTTLS upgrade on the submission port (587).
    #[default]
    Starttls,
    /// Implicit TLS (465).
    Implicit,
}

fn default_smtp_port() -> u16 {
    587
}
fn default_imap_port() -> u16 {
    993
}

/// `[channels.email]` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailChannelConfig {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_pass: String,
    pub from_addr: String,
    #[serde(default)]
    pub smtp_tls: SmtpTls,
    // ── Inbound (IMAP) — consumed by the PENDING-LIVE poll loop. ──
    #[serde(default)]
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default)]
    pub imap_user: String,
    #[serde(default)]
    pub imap_pass: String,
}

/// A parsed inbound email reduced to what the reply pipeline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEmail {
    pub from: String,
    pub subject: String,
    pub body: String,
    pub message_id: Option<String>,
}

/// Split a raw message into `(header_block, body)` on the first blank line
/// (tolerating CRLF or LF). No blank line ⇒ all headers, empty body.
fn split_headers_body(raw: &str) -> (&str, &str) {
    if let Some(idx) = raw.find("\r\n\r\n") {
        (&raw[..idx], &raw[idx + 4..])
    } else if let Some(idx) = raw.find("\n\n") {
        (&raw[..idx], &raw[idx + 2..])
    } else {
        (raw, "")
    }
}

/// Parse a header block into `(name, value)` pairs, unfolding continuation
/// lines (a line starting with space/tab continues the previous header).
fn parse_headers(block: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in block.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            // Folded continuation of the previous header value.
            let last = out.last_mut().unwrap();
            last.1.push(' ');
            last.1.push_str(line.trim());
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

/// Parse a raw RFC822 message into an [`InboundEmail`]: the
/// `From`/`Subject`/`Message-ID` headers + the plain-text body (everything after
/// the first blank line). Deliberately minimal — no MIME multipart decode — so
/// it is dependency-free and deterministic; multipart/HTML handling is a
/// follow-up for when the IMAP transport lands.
pub fn parse_inbound(raw: &str) -> InboundEmail {
    let (header_block, body) = split_headers_body(raw);
    let headers = parse_headers(header_block);
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    InboundEmail {
        from: get("From").unwrap_or_default(),
        subject: get("Subject").unwrap_or_default(),
        body: body.trim().to_string(),
        message_id: get("Message-ID"),
    }
}

/// Build a `lettre` message (factored out of [`send_email`] so header
/// construction is unit-testable without a live server).
pub fn build_message(
    from_addr: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<lettre::Message> {
    lettre::Message::builder()
        .from(
            from_addr
                .parse()
                .map_err(|e| DuDuClawError::Channel(format!("bad from addr '{from_addr}': {e}")))?,
        )
        .to(to
            .parse()
            .map_err(|e| DuDuClawError::Channel(format!("bad to addr '{to}': {e}")))?)
        .subject(subject)
        .body(body.to_string())
        .map_err(|e| DuDuClawError::Channel(format!("build message: {e}")))
}

/// Send an email via the configured SMTP server (async, rustls). Loopback
/// plaintext (`SmtpTls::None`) is what the `smtp_send_loopback` test exercises.
pub async fn send_email(
    cfg: &EmailChannelConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    let email = build_message(&cfg.from_addr, to, subject, body)?;

    let builder = match cfg.smtp_tls {
        SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.smtp_host),
        SmtpTls::Starttls => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)
                .map_err(|e| DuDuClawError::Channel(format!("smtp starttls setup: {e}")))?
        }
        SmtpTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)
            .map_err(|e| DuDuClawError::Channel(format!("smtp relay setup: {e}")))?,
    }
    .port(cfg.smtp_port);

    let builder = if !cfg.smtp_user.is_empty() {
        builder.credentials(Credentials::new(
            cfg.smtp_user.clone(),
            cfg.smtp_pass.clone(),
        ))
    } else {
        builder
    };

    builder
        .build()
        .send(email)
        .await
        .map_err(|e| DuDuClawError::Channel(format!("smtp send failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inbound_extracts_headers_and_body() {
        let raw = "From: alice@example.com\r\n\
                   Subject: Hello there\r\n\
                   Message-ID: <abc123@example.com>\r\n\
                   \r\n\
                   This is the body.\r\nSecond line.\r\n";
        let m = parse_inbound(raw);
        assert_eq!(m.from, "alice@example.com");
        assert_eq!(m.subject, "Hello there");
        assert_eq!(m.message_id.as_deref(), Some("<abc123@example.com>"));
        assert_eq!(m.body, "This is the body.\r\nSecond line.");
    }

    #[test]
    fn parse_inbound_unfolds_continuation_headers() {
        // A folded Subject (RFC 5322 §2.2.3): continuation line starts with WSP.
        let raw = "From: bob@example.com\nSubject: a very long\n  subject line\n\nbody\n";
        let m = parse_inbound(raw);
        assert_eq!(m.subject, "a very long subject line");
        assert_eq!(m.from, "bob@example.com");
        assert_eq!(m.body, "body");
    }

    #[test]
    fn parse_inbound_tolerates_missing_headers_and_body() {
        let m = parse_inbound("From: x@y.z\n");
        assert_eq!(m.from, "x@y.z");
        assert_eq!(m.subject, "");
        assert_eq!(m.body, "");
        assert!(m.message_id.is_none());
    }

    #[test]
    fn config_defaults_apply() {
        let cfg: EmailChannelConfig = toml::from_str(
            r#"
            smtp_host = "smtp.example.com"
            from_addr = "bot@example.com"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.smtp_port, 587, "default submission port");
        assert_eq!(cfg.imap_port, 993, "default imaps port");
        assert_eq!(cfg.smtp_tls, SmtpTls::Starttls, "default STARTTLS");
    }

    #[test]
    fn build_message_produces_valid_headers() {
        let msg = build_message(
            "Bot <bot@example.com>",
            "user@example.com",
            "Subject line",
            "hello",
        )
        .unwrap();
        let bytes = msg.formatted();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("From: Bot <bot@example.com>"));
        assert!(text.contains("To: user@example.com"));
        assert!(text.contains("Subject: Subject line"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn build_message_rejects_bad_address() {
        assert!(build_message("not-an-email", "u@e.com", "s", "b").is_err());
    }

    /// Live SMTP send against a loopback server. Run with the server up:
    ///   ./scripts/smtp-loopback.py &        # or the venv aiosmtpd one-liner
    ///   cargo test -p duduclaw-gateway --lib email::tests::smtp_send_loopback -- --ignored
    #[tokio::test]
    #[ignore]
    async fn smtp_send_loopback() {
        let cfg = EmailChannelConfig {
            smtp_host: "127.0.0.1".to_string(),
            smtp_port: 2525,
            smtp_user: String::new(),
            smtp_pass: String::new(),
            from_addr: "bot@example.com".to_string(),
            smtp_tls: SmtpTls::None,
            imap_host: String::new(),
            imap_port: 993,
            imap_user: String::new(),
            imap_pass: String::new(),
        };
        send_email(&cfg, "operator@example.com", "loopback test", "it works")
            .await
            .expect("send to loopback aiosmtpd should succeed");
    }
}
