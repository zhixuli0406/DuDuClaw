//! Open a filesystem path or an http(s) URL with the OS default handler.
//!
//! macOS: `open`. Linux: `xdg-open`. Other: [`OpenError::Unsupported`].
//!
//! This is an **action** primitive; the MCP layer stacks capability + ActionGuard
//! gates on top (see `os_open` in `duduclaw-cli`). The security-relevant piece
//! here is [`classify_target`], a fail-closed classifier:
//! - only the exact `http://` / `https://` prefixes are treated as URLs
//!   (case-insensitive, never a substring `contains` — convention #2);
//! - any other explicit URI scheme (`file:`, `javascript:`, `data:`, `ftp:`, …)
//!   is rejected;
//! - a scheme-less string is a path whose existence is enforced via
//!   `canonicalize()` before the handler is spawned. A leading `~` / `~/` is
//!   expanded against the user home via `duduclaw_core::expand_tilde` first —
//!   `canonicalize()` does not expand `~`, so an unexpanded home path would
//!   spuriously fail with `PathNotFound`.

use std::path::PathBuf;
use std::time::Duration;

/// Classification of an `os_open` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenTarget {
    /// An allowed http(s) URL (verbatim, as given).
    Url(String),
    /// A filesystem path (not yet canonicalized).
    Path(PathBuf),
}

/// Errors opening a target.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("empty target")]
    Empty,
    #[error(
        "disallowed URL scheme (only http/https URLs and existing file paths are allowed): {0}:"
    )]
    DisallowedScheme(String),
    #[error("path does not exist: {0}")]
    PathNotFound(String),
    #[error("open helper spawn failed: {0}")]
    Spawn(String),
    #[error("open helper timed out")]
    Timeout,
    #[error("open helper exited with status {code}: {stderr}")]
    Failed { code: i32, stderr: String },
    #[error("open is not supported on this platform")]
    Unsupported,
}

/// Extract an RFC-3986-ish scheme from a target, if one is present.
///
/// Returns `None` for scheme-less strings and for Windows drive letters
/// (`C:\…`), which are single-character and thus excluded by the length-≥2
/// rule so they classify as paths rather than schemes. Uses `split_once(':')`
/// (char-boundary safe) rather than byte indexing (convention #1).
fn scheme_of(t: &str) -> Option<&str> {
    let (scheme, _rest) = t.split_once(':')?;
    if scheme.len() < 2 {
        return None;
    }
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme)
}

/// Classify a target as an allowed URL, a path, or a rejected scheme.
/// Fail-closed: anything with an explicit non-http(s) scheme is denied.
pub fn classify_target(target: &str) -> Result<OpenTarget, OpenError> {
    let t = target.trim();
    if t.is_empty() {
        return Err(OpenError::Empty);
    }

    // Exact-prefix allow for http(s), case-insensitive (convention #2).
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(OpenTarget::Url(t.to_string()));
    }

    // Any other explicit URI scheme is rejected.
    if let Some(scheme) = scheme_of(t) {
        return Err(OpenError::DisallowedScheme(scheme.to_ascii_lowercase()));
    }

    // Scheme-less → filesystem path (existence checked at open time). A leading
    // `~` / `~/` is expanded here so the later `canonicalize()` — which does not
    // itself expand `~` — can resolve home-relative paths.
    Ok(OpenTarget::Path(duduclaw_core::expand_tilde(t)))
}

/// Open a path or http(s) URL with the OS default handler.
pub async fn open_path_or_url(target: &str) -> Result<(), OpenError> {
    match classify_target(target)? {
        OpenTarget::Url(url) => open_with_helper(&url).await,
        OpenTarget::Path(p) => {
            let canon = p
                .canonicalize()
                .map_err(|_| OpenError::PathNotFound(p.to_string_lossy().into_owned()))?;
            open_with_helper(&canon.to_string_lossy()).await
        }
    }
}

/// Spawn the platform "open" helper, wait ≤5s, and map exit status.
async fn open_with_helper(arg: &str) -> Result<(), OpenError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        /// Helper timeout — a hung `open` must not stall the caller.
        const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

        #[cfg(target_os = "macos")]
        let bin = "open";
        #[cfg(target_os = "linux")]
        let bin = "xdg-open";

        let fut = tokio::process::Command::new(bin).arg(arg).output();
        let output = match tokio::time::timeout(OPEN_TIMEOUT, fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(OpenError::Spawn(format!("{bin}: {e}"))),
            Err(_) => return Err(OpenError::Timeout),
        };
        if output.status.success() {
            Ok(())
        } else {
            let code = output.status.code().unwrap_or(-1);
            let stderr =
                duduclaw_core::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 240);
            Err(OpenError::Failed { code, stderr })
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (arg, Duration::from_secs(0));
        Err(OpenError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_http_and_https_any_case() {
        assert!(matches!(
            classify_target("http://example.com"),
            Ok(OpenTarget::Url(_))
        ));
        assert!(matches!(
            classify_target("https://example.com/x?y=1"),
            Ok(OpenTarget::Url(_))
        ));
        assert!(matches!(
            classify_target("HTTPS://Example.COM"),
            Ok(OpenTarget::Url(_))
        ));
    }

    #[test]
    fn rejects_dangerous_schemes() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ftp://host/f",
            "smb://host/share",
            "vscode://x",
            "mailto:a@b.c",
        ] {
            let r = classify_target(bad);
            assert!(
                matches!(r, Err(OpenError::DisallowedScheme(_))),
                "expected {bad} to be rejected, got {r:?}"
            );
        }
    }

    #[test]
    fn empty_is_rejected() {
        assert!(matches!(classify_target("   "), Err(OpenError::Empty)));
    }

    #[test]
    fn scheme_less_is_a_path() {
        assert!(matches!(
            classify_target("/home/u/report.pdf"),
            Ok(OpenTarget::Path(_))
        ));
        assert!(matches!(
            classify_target("relative/dir/file.txt"),
            Ok(OpenTarget::Path(_))
        ));
    }

    #[test]
    fn tilde_path_is_expanded_to_home() {
        // A leading `~` classifies as a Path whose value is expanded against the
        // user home (so the later canonicalize can resolve it). Assert against
        // the live home_dir() rather than mutating $HOME (avoids racing parallel
        // tests / the workspace `unsafe_code` lint).
        let home = PathBuf::from(duduclaw_core::home_dir());
        match classify_target("~/Documents/x.md") {
            Ok(OpenTarget::Path(p)) => assert_eq!(p, home.join("Documents/x.md")),
            other => panic!("expected an expanded Path, got {other:?}"),
        }
    }

    #[test]
    fn windows_drive_letter_is_a_path_not_a_scheme() {
        // Single-letter "scheme" is excluded, so this is a path, not rejected.
        assert!(matches!(
            classify_target("C:\\Users\\me\\file.txt"),
            Ok(OpenTarget::Path(_))
        ));
    }

    #[tokio::test]
    async fn open_nonexistent_path_errors() {
        let r = open_path_or_url("/definitely/not/here/duduclaw-os-xyz.pdf").await;
        assert!(matches!(r, Err(OpenError::PathNotFound(_))));
    }

    #[tokio::test]
    async fn open_disallowed_scheme_errors_before_spawn() {
        let r = open_path_or_url("javascript:alert(1)").await;
        assert!(matches!(r, Err(OpenError::DisallowedScheme(_))));
    }
}
