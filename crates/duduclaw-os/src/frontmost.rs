//! Frontmost (foreground) application + window title, via OS shell-out.
//!
//! macOS: `osascript` driving `System Events` (AppleScript, not JXA — a plain
//! two-line query, no external data is interpolated so there is no injection
//! surface here). Linux: `xdotool getactivewindow getwindowname` (chained
//! xdotool sub-commands; missing binary → [`FrontmostError::Unsupported`]).
//! Windows: [`FrontmostError::Unsupported`] (no vendored win32 binding per the
//! crate's "shell-out only, no objc/win32" convention).
//!
//! Sensing priority (research doc §②-6, "structured API over pixels"): this is
//! a structured `System Events` query, not a screenshot/OCR fallback.

use std::time::Duration;

use serde::Serialize;

/// Hard cap on the app name / window title fields (codepoints, CJK-safe) —
/// mirrors `notify_native::MAX_FIELD_CHARS` so this stays consistent with the
/// other OS-native text fields that flow into autopilot rule conditions.
const MAX_FIELD_CHARS: usize = 200;

/// Helper subprocess timeout — a hung `osascript`/`xdotool` must not stall the
/// caller (matches `notify_native`'s / `open_target`'s 5s budget).
const HELPER_TIMEOUT: Duration = Duration::from_secs(5);

/// The frontmost application name and (if any) its focused window title.
/// Both fields are already codepoint-truncated to [`MAX_FIELD_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontmostInfo {
    pub app: String,
    pub window_title: String,
}

/// Errors detecting the frontmost app/window.
#[derive(Debug, thiserror::Error)]
pub enum FrontmostError {
    #[error("frontmost detection is not supported on this platform")]
    Unsupported,
    #[error("frontmost helper spawn failed: {0}")]
    Spawn(String),
    #[error("frontmost helper timed out")]
    Timeout,
    /// macOS System Events automation permission has not been granted (TCC).
    /// Recognized from a known `osascript` error-text/code marker — see
    /// [`classify_stderr`]. `duduclaw os doctor` surfaces this with a
    /// zh-TW remediation pointer; callers must NOT attempt to bypass it.
    #[error("automation permission denied: {0}")]
    PermissionDenied(String),
    #[error("frontmost helper exited with status {code}: {stderr}")]
    Failed { code: i32, stderr: String },
    #[error("failed to parse frontmost helper output: {0}")]
    ParseError(String),
}

/// Detect the current frontmost application and its focused window title.
pub async fn frontmost_info() -> Result<FrontmostInfo, FrontmostError> {
    #[cfg(target_os = "macos")]
    {
        // Two-line AppleScript query against System Events. No external data
        // is interpolated (the script is a fixed literal), so there is no
        // AppleScript-injection surface to sanitize — unlike `notify_native`,
        // which interpolates LLM-provided text.
        const SCRIPT_APP: &str = r#"tell application "System Events" to get name of first application process whose frontmost is true"#;
        const SCRIPT_WINDOW: &str = r#"tell application "System Events"
	set frontApp to first application process whose frontmost is true
	try
		return name of front window of frontApp
	on error
		return ""
	end try
end tell"#;

        let app_out = run_osascript(&["-e", SCRIPT_APP]).await?;
        // A window-title lookup failure (e.g. the frontmost app has no
        // windows, such as a background agent) is not fatal — fall back to an
        // empty title rather than failing the whole call.
        let title_out = run_osascript(&["-e", SCRIPT_WINDOW])
            .await
            .unwrap_or_default();

        Ok(FrontmostInfo {
            app: duduclaw_core::truncate_chars(app_out.trim(), MAX_FIELD_CHARS),
            window_title: duduclaw_core::truncate_chars(title_out.trim(), MAX_FIELD_CHARS),
        })
    }
    #[cfg(target_os = "linux")]
    {
        // `xdotool getactivewindow getwindowname` chains two xdotool
        // sub-commands (the active window id feeds the title lookup) in one
        // invocation. xdotool has no direct concept of "owning application
        // name" as a single query, so `app` is left empty here — the window
        // title is the structured signal Linux exposes cheaply.
        let out = run_helper(
            "xdotool",
            &["getactivewindow", "getwindowname"],
            HELPER_TIMEOUT,
        )
        .await?;
        Ok(FrontmostInfo {
            app: String::new(),
            window_title: duduclaw_core::truncate_chars(out.trim(), MAX_FIELD_CHARS),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(FrontmostError::Unsupported)
    }
}

/// Run `osascript -e <line>` (each element of `args` is its own argv entry —
/// never a shell-interpolated string) and return trimmed stdout, classifying
/// TCC permission denial via [`classify_stderr`].
#[cfg(target_os = "macos")]
async fn run_osascript(args: &[&str]) -> Result<String, FrontmostError> {
    run_helper("osascript", args, HELPER_TIMEOUT).await
}

/// Spawn an OS helper, wait ≤`timeout`, and map its exit status to a
/// [`FrontmostError`]. Shared by the macOS and Linux branches.
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run_helper(bin: &str, args: &[&str], timeout: Duration) -> Result<String, FrontmostError> {
    let fut = tokio::process::Command::new(bin).args(args).output();
    let output = match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(FrontmostError::Spawn(format!("{bin}: {e}"))),
        Err(_) => return Err(FrontmostError::Timeout),
    };
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr =
            duduclaw_core::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 240);
        Err(classify_stderr(code, stderr))
    }
}

/// Classify a failed helper's exit code + stderr into a [`FrontmostError`],
/// recognizing the macOS System Events automation-permission (TCC) denial so
/// `duduclaw os doctor` can tell "not granted yet" apart from "genuinely
/// broke". This is a diagnostic/UX classification, not a security gate — a
/// misclassification only changes which hint text is shown, never an
/// authorization decision — so a plain case-insensitive substring match is an
/// acceptable exception to the routing-decision `contains` rule (project
/// convention #2 governs security/routing decisions).
fn classify_stderr(code: i32, stderr: String) -> FrontmostError {
    let lower = stderr.to_ascii_lowercase();
    let is_permission_denied = lower.contains("not authorized")
        || lower.contains("assistive access")
        || lower.contains("-1743")
        || lower.contains("(-25211)");
    if is_permission_denied {
        FrontmostError::PermissionDenied(stderr)
    } else {
        FrontmostError::Failed { code, stderr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_stderr_detects_tcc_denial_markers() {
        for sample in [
            "System Events got an error: osascript is not allowed assistive access.",
            "execution error: Not authorized to send Apple events to System Events. (-1743)",
            "osascript error (-25211)",
        ] {
            let e = classify_stderr(1, sample.to_string());
            assert!(
                matches!(e, FrontmostError::PermissionDenied(_)),
                "expected PermissionDenied for {sample:?}, got {e:?}"
            );
        }
    }

    #[test]
    fn classify_stderr_generic_failure_is_not_permission_denied() {
        let e = classify_stderr(1, "System Events got an error: doesn't understand".into());
        assert!(matches!(e, FrontmostError::Failed { code: 1, .. }));
    }

    #[test]
    fn truncates_by_codepoint_not_byte() {
        let long: String = "視".repeat(250);
        let out = duduclaw_core::truncate_chars(&long, MAX_FIELD_CHARS);
        assert_eq!(out.chars().count(), MAX_FIELD_CHARS);
    }
}
