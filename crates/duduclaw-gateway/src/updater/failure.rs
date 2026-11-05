//! Stable dashboard error kinds around the existing String-based provider API.
//! Classify the original reason, but expose only bounded, sanitized diagnostics.
pub fn failure_kind(detail: &str) -> &'static str {
    let text = detail.to_ascii_lowercase();
    if text.contains("rollback failed") { "rollback" }
    else if text.contains("size limit") || text.contains("too large") || text.contains("size overflow") { "size" }
    else if text.starts_with("update download failed:")
        || ["failed to read update archive:", "failed to read checksum file:", "failed to read signature file:"]
            .iter().any(|prefix| text.starts_with(prefix)) { "download" }
    else if text.starts_with("update verification failed:") { "verify" }
    else if text.contains("staging write failed") || text.contains("failed to stage") || text.contains("failed to sync") || text.contains("failed to set permissions") { "write" }
    else if text.starts_with("update extraction failed:") { "extract" }
    else if text.contains("failed to backup") || text.contains("failed to install") { "swap" }
    else if text.contains("download") || text.contains("http") { "download" }
    else if text.contains("verification") || text.contains("cannot execute new binary") || text.contains("checksum") || text.contains("signature") { "verify" }
    else { "unknown" }
}

// Keep sanitization at the shared wire/audit boundary, not only in the UI.
const MAX_FAILURE_DETAIL_CHARS: usize = 1024;

pub fn sanitize_failure_detail(detail: &str) -> String {
    use std::sync::OnceLock;
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    static REDACT: OnceLock<regex::Regex> = OnceLock::new();
    let urls = URL.get_or_init(|| regex::Regex::new(
        r#"(?i)[a-z][a-z0-9+.-]+://[^\s<>"']+"#,
    ).expect("static update URL regex"));
    // One pass prevents a secret-shaped substring inside a path from exposing
    // the remaining path, or a slash inside a base64 token from splitting it.
    let redact = REDACT.get_or_init(|| regex::Regex::new(
        r#"(?ix)
        (?P<path>
          (?:[a-z]:[\\/]|\\|/|~[a-z0-9_-]*[\\/]|\$\{?HOME\}?[\\/]|%(?:USERPROFILE|LOCALAPPDATA|APPDATA)%[\\/])
          [^<>"'\r\n:|]*
        )
        | \b(?:bearer|basic)\s+(?:"[^"]*"|'[^']*'|[^\s,;"']+)
        | ["']?\b(?:token|key|api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|authorization)
          ["']?\s*[:=]\s*(?:(?:bearer|basic)\s+[^\s,;"']+|"[^"]*"|'[^']*'|[^\s,;]+)
        | [a-z0-9_+/=-]{32,}
        "#,
    ).expect("static update redaction regex"));
    let clean_text = |text: &str| {
        redact.replace_all(text, |caps: &regex::Captures<'_>| {
            if caps.name("path").is_some() { "[path]" } else { "[redacted]" }
        }).into_owned()
    };
    // Process URLs separately so their retained origin is not mistaken for a path.
    // Normalize controls first so hidden delimiters cannot survive into the wire.
    let normalized: String = detail.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut sanitized = String::new();
    let mut offset = 0;
    for found in urls.find_iter(&normalized) {
        sanitized.push_str(&clean_text(&normalized[offset..found.start()]));
        let origin = url::Url::parse(found.as_str()).ok().and_then(|url| {
            if url.scheme() == "file" { return None; }
            url.host().map(|host| format!("{}://{}", url.scheme(), host))
        });
        sanitized.push_str(&origin.unwrap_or_else(|| "[url]".into()));
        offset = found.end();
    }
    sanitized.push_str(&clean_text(&normalized[offset..]));
    // chars() keeps multibyte diagnostics valid; the ellipsis is inside the cap.
    if sanitized.chars().count() > MAX_FAILURE_DETAIL_CHARS {
        sanitized = sanitized.chars().take(MAX_FAILURE_DETAIL_CHARS - 1).collect();
        sanitized.push('…');
    }
    sanitized
}

pub fn failure_payload(detail: &str) -> serde_json::Value {
    let kind = failure_kind(detail);
    let detail = sanitize_failure_detail(detail);
    serde_json::json!({
        "code": "update_failed",
        "kind": kind,
        "message": detail,
        "detail": detail,
    })
}

pub fn failure_response(detail: &str) -> crate::protocol::WsFrame {
    crate::protocol::WsFrame::Response {
        id: String::new(), ok: false, payload: None, error: Some(failure_payload(detail)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_details_hide_paths_and_keep_diagnostic_reason() {
        for path in [
            "/Users/alice/Library/Application Support/DuDuClaw/backup",
            "/opt/duduclaw/bin/duduclaw.bak", "/home/alice/.duduclaw",
            "~/private/backup", "~alice/private/backup", "$HOME/private/backup",
            "${HOME}/private/backup", "%USERPROFILE%\\private\\backup",
            r"C:\Program Files\DuDuClaw\duduclaw.exe",
            r"C:\Program Files (Private Client)\DuDuClaw\duduclaw.exe",
            r"\Windows\private\backup", r"C://Users/alice/backup",
            "/home/alice/private,client;(backup)/duduclaw",
            r"C:/Users/alice/AppData/Local/DuDuClaw/backup",
            r"\\server\private share\backup", "/tmp/update",
        ] {
            let reason = format!("Failed to backup current binary '{path}': denied");
            let expected = "Failed to backup current binary '[path]': denied";
            assert_eq!(sanitize_failure_detail(&reason), expected, "{path}");
            assert_eq!(sanitize_failure_detail(&format!("Failed to install {path}: disk full")),
                "Failed to install [path]: disk full", "{path}");
        }
    }

    #[test]
    fn failure_details_reduce_urls_and_remove_secrets() {
        assert_eq!(sanitize_failure_detail(
            "Update download failed: https://alice:password@updates.example:8443/private/file?token=short#secret"
        ), "Update download failed: https://updates.example");
        assert_eq!(sanitize_failure_detail("GET http://[::1]:8080/private?key=value"),
            "GET http://[::1]");
        assert_eq!(sanitize_failure_detail("file:///Users/alice/private"), "[url]");
        for secret in [
            "Bearer short.jwt.value", "bEaReR abcdef", "token=short", "key=short",
            "api_key=short", "access_token=short", "secret: 'short value'",
            "Authorization: Bearer short", "Authorization: Basic dXNlcjpwYXNz",
            "Bearer \"short\"", "Bearer 'short'",
            r#""password": "short value""#,
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            "dGhpc0lzQVNlY3JldFRva2VuRm9yVGVzdGluZw==",
            "aBcDeFgHiJkLmNoPqRsTuVwXyZ01234-_",
        ] {
            assert_eq!(sanitize_failure_detail(&format!("denied {secret}")),
                "denied [redacted]", "{secret}");
        }
    }

    #[test]
    fn failure_details_are_bounded_and_safe_in_rpc_and_push() {
        let reason = format!("Update download failed: {}", "錯誤".repeat(2000));
        let sanitized = sanitize_failure_detail(&reason);
        assert_eq!(sanitized.chars().count(), MAX_FAILURE_DETAIL_CHARS);
        assert!(sanitized.ends_with('…'));
        assert!(!sanitize_failure_detail("failed\n\r\0\x1b\tdenied").chars().any(char::is_control));
        let reason = "Failed to install AND rollback failed: /home/alice/backup: token=secret";
        let payload = failure_payload(reason);
        assert_eq!(payload["kind"], "rollback");
        assert_eq!(payload["message"], payload["detail"]);
        let wire = serde_json::to_value(failure_response(reason)).unwrap();
        assert_eq!(wire["error"], payload);
        let event = crate::protocol::WsFrame::event("system.update_failed", payload.clone());
        assert_eq!(serde_json::to_value(event).unwrap()["payload"], payload);
        let serialized = payload.to_string();
        assert!(!serialized.contains("alice"));
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn update_failure_preserves_each_stage_in_rpc_and_event_payloads() {
        for (reason, kind) in [
            ("Update download failed: connection reset", "download"),
            ("Failed to read Update archive: reset", "download"),
            ("Failed to read Checksum file: reset", "download"),
            ("Failed to read Signature file: reset", "download"),
            ("Update verification failed: SHA-256 checksum mismatch", "verify"),
            ("Update extraction failed: Extracted binary exceeds size limit", "size"),
            ("Update extraction failed: Archive read failed: corrupt gzip", "extract"),
            ("Update extraction failed: Staging write failed: disk full", "write"),
            ("Failed to sync binary: disk full", "write"),
            ("Failed to backup current binary: denied", "swap"),
            ("Failed to install new binary (rolled back successfully): denied", "swap"),
            ("Failed to install AND rollback failed: denied", "rollback"),
            ("Provider refused this release", "unknown"),
        ] {
            let payload = failure_payload(reason);
            assert_eq!(payload["kind"], kind);
            assert_eq!(payload["detail"], reason);
            let frame = failure_response(reason);
            let wire = serde_json::to_value(frame).unwrap();
            assert_eq!(wire["error"]["message"], reason);
            let event = crate::protocol::WsFrame::event("system.update_failed", payload);
            assert_eq!(serde_json::to_value(event).unwrap()["payload"]["detail"], reason);
        }
    }
}
