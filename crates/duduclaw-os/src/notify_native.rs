//! Native desktop notifications via OS shell-out.
//!
//! macOS: `osascript -e 'display notification "…" with title "…"'`.
//! Linux: `notify-send <title> <body>`.
//! Other: [`NotifyError::Unsupported`].
//!
//! **Injection defence** (the title/body may originate from an LLM tool call):
//! both fields are codepoint-truncated (CJK-safe) and stripped of the only
//! characters that can break out of an AppleScript double-quoted literal or
//! inject a new statement — `"`, `\`, and newlines (→ space). See
//! [`sanitize_osascript`].

use std::time::Duration;

/// Hard cap on notification title / body length (codepoints, CJK-safe).
const MAX_FIELD_CHARS: usize = 200;

/// Errors sending a native notification.
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("native notifications are not supported on this platform")]
    Unsupported,
    #[error("notification helper spawn failed: {0}")]
    Spawn(String),
    #[error("notification helper timed out")]
    Timeout,
    #[error("notification helper exited with status {code}: {stderr}")]
    Failed { code: i32, stderr: String },
}

/// Sanitize a user/LLM-provided field for safe interpolation into an
/// AppleScript string literal.
///
/// Steps (order matters): codepoint-truncate to [`MAX_FIELD_CHARS`] via
/// `duduclaw_core::truncate_chars` (never a raw byte slice — convention #1),
/// then drop `"` / `\` and fold newlines/carriage-returns to spaces. The result
/// contains no character that can terminate the literal or start a new
/// AppleScript statement, so the interpolation in [`send_notification`] is safe.
pub fn sanitize_osascript(s: &str) -> String {
    let truncated = duduclaw_core::truncate_chars(s, MAX_FIELD_CHARS);
    truncated
        .chars()
        .filter_map(|c| match c {
            '"' | '\\' => None,
            '\n' | '\r' => Some(' '),
            _ => Some(c),
        })
        .collect()
}

/// Send a native desktop notification. `title` / `body` are sanitized before use.
pub async fn send_notification(title: &str, body: &str) -> Result<(), NotifyError> {
    let title = sanitize_osascript(title);
    let body = sanitize_osascript(body);

    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification \"{body}\" with title \"{title}\"");
        run_helper("osascript", &["-e", &script]).await
    }
    #[cfg(target_os = "linux")]
    {
        run_helper("notify-send", &[title.as_str(), body.as_str()]).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body);
        Err(NotifyError::Unsupported)
    }
}

/// Spawn an OS helper, wait ≤5s, and map its exit status to a [`NotifyError`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run_helper(bin: &str, args: &[&str]) -> Result<(), NotifyError> {
    /// Helper timeout — a hung `osascript` must not stall the caller.
    const HELPER_TIMEOUT: Duration = Duration::from_secs(5);

    let fut = tokio::process::Command::new(bin).args(args).output();
    let output = match tokio::time::timeout(HELPER_TIMEOUT, fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(NotifyError::Spawn(format!("{bin}: {e}"))),
        Err(_) => return Err(NotifyError::Timeout),
    };
    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr =
            duduclaw_core::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 240);
        Err(NotifyError::Failed { code, stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_quotes_and_backslashes() {
        // Classic AppleScript break-out attempt.
        let evil = r#"" & (do shell script "rm -rf ~") & ""#;
        let out = sanitize_osascript(evil);
        assert!(!out.contains('"'), "double-quote must be removed: {out}");
        assert!(!out.contains('\\'), "backslash must be removed: {out}");
    }

    #[test]
    fn sanitize_folds_newlines_to_space() {
        let out = sanitize_osascript("line1\nline2\rline3");
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert_eq!(out, "line1 line2 line3");
    }

    #[test]
    fn sanitize_preserves_cjk_and_truncates_by_codepoint() {
        // 250 CJK codepoints → truncated to 200, no panic on multi-byte chars.
        let long: String = "測".repeat(250);
        let out = sanitize_osascript(&long);
        assert_eq!(out.chars().count(), MAX_FIELD_CHARS);
        assert!(out.chars().all(|c| c == '測'));
    }

    #[test]
    fn sanitize_keeps_normal_text() {
        let out = sanitize_osascript("Build finished ✅ 建置完成");
        assert_eq!(out, "Build finished ✅ 建置完成");
    }

    #[test]
    fn sanitize_mixed_injection_sample() {
        let out = sanitize_osascript("title\"; display notification \"pwned\\");
        assert!(!out.contains('"'));
        assert!(!out.contains('\\'));
        assert!(!out.contains('\n'));
    }
}
