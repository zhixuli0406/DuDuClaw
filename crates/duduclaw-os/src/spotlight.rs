//! Spotlight metadata search (`mdfind`), macOS only.
//!
//! Sensing priority (research doc §②-6, "structured API over pixels"): `mdfind`
//! queries the macOS metadata index — a structured API, not a filesystem walk
//! or screen scrape.
//!
//! **Injection defence**: the query and scope directory are always passed as
//! discrete `argv` elements via `tokio::process::Command::args` — never
//! interpolated into a shell string. There is no shell in the call path at
//! all, so classic `;`/`` ` ``/`$()` shell-metacharacter injection is
//! structurally impossible here (the OS exec syscall receives an argv array
//! directly).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default result cap when the caller doesn't specify one.
pub const DEFAULT_LIMIT: usize = 20;
/// Hard ceiling on the result cap — an MCP caller can request fewer, never a
/// runaway number of rows (defence against a pathological `limit` value
/// blowing up the tool response).
pub const MAX_LIMIT: usize = 200;

/// Helper subprocess timeout. `mdfind` normally returns near-instantly (it
/// queries a live index), but a cold/rebuilding index can stall — bounded the
/// same way as the other `duduclaw-os` shell-outs.
const MDFIND_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors running a Spotlight search.
#[derive(Debug, thiserror::Error)]
pub enum SpotlightError {
    #[error("spotlight search is only supported on macOS")]
    Unsupported,
    #[error("empty query")]
    EmptyQuery,
    #[error("scope directory does not exist: {0}")]
    ScopeNotFound(String),
    #[error("mdfind spawn failed: {0}")]
    Spawn(String),
    #[error("mdfind timed out")]
    Timeout,
    #[error("mdfind exited with status {code}: {stderr}")]
    Failed { code: i32, stderr: String },
}

/// Build the `mdfind` argv for a query, optionally scoped to a directory.
/// Pure and platform-independent so it is directly unit-testable without a
/// live `mdfind` binary. Each returned element is a single argv entry — no
/// element is ever a composed/interpolated shell string.
fn build_args(query: &str, scope_dir: Option<&Path>) -> Vec<String> {
    let mut args = Vec::with_capacity(3);
    if let Some(dir) = scope_dir {
        args.push("-onlyin".to_string());
        args.push(dir.to_string_lossy().into_owned());
    }
    // The query is pushed verbatim as ONE argv element, whatever characters
    // it contains (quotes, semicolons, backticks, …) — there is no shell to
    // break out of.
    args.push(query.to_string());
    args
}

/// Parse raw `mdfind` stdout (one path per line) into a list of non-empty,
/// trimmed path strings. Pure function — canonicalization (which needs
/// filesystem access) happens in [`spotlight_search`].
fn parse_stdout(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Run a Spotlight metadata search.
///
/// `query` is passed to `mdfind` as-is (Spotlight's own query syntax — plain
/// text terms work as a substring/word search). `scope_dir`, when given, must
/// exist (fail-closed: a non-existent scope is an error, not a silently
/// unscoped search). `limit` defaults to [`DEFAULT_LIMIT`] and is clamped to
/// [`MAX_LIMIT`]. Every returned path is canonicalized; entries that vanish
/// between the search and canonicalization (e.g. deleted concurrently) are
/// silently skipped — a raced deletion is not treated as a wholesale error.
pub async fn spotlight_search(
    query: &str,
    scope_dir: Option<&Path>,
    limit: Option<usize>,
) -> Result<Vec<PathBuf>, SpotlightError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(SpotlightError::EmptyQuery);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (query, scope_dir, limit);
        return Err(SpotlightError::Unsupported);
    }

    #[cfg(target_os = "macos")]
    {
        let canon_scope =
            match scope_dir {
                Some(dir) => Some(dir.canonicalize().map_err(|_| {
                    SpotlightError::ScopeNotFound(dir.to_string_lossy().into_owned())
                })?),
                None => None,
            };
        let args = build_args(query, canon_scope.as_deref());
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();

        let fut = tokio::process::Command::new("mdfind")
            .args(&args_ref)
            .output();
        let output = match tokio::time::timeout(MDFIND_TIMEOUT, fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(SpotlightError::Spawn(format!("mdfind: {e}"))),
            Err(_) => return Err(SpotlightError::Timeout),
        };
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr =
                duduclaw_core::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 240);
            return Err(SpotlightError::Failed { code, stderr });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let cap = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let hits: Vec<PathBuf> = parse_stdout(&stdout)
            .into_iter()
            .filter_map(|p| PathBuf::from(p).canonicalize().ok())
            .take(cap)
            .collect();
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_unscoped() {
        let args = build_args("invoice 2026", None);
        assert_eq!(args, vec!["invoice 2026".to_string()]);
    }

    #[test]
    fn build_args_scoped() {
        let args = build_args("report", Some(Path::new("/Users/me/Documents")));
        assert_eq!(
            args,
            vec![
                "-onlyin".to_string(),
                "/Users/me/Documents".to_string(),
                "report".to_string(),
            ]
        );
    }

    #[test]
    fn build_args_preserves_shell_metacharacters_verbatim() {
        // There is no shell in the call path, so a query containing shell
        // metacharacters is passed through unchanged as a single argv
        // element — it is never given the chance to break out.
        let evil = "x; rm -rf ~ `whoami` $(id)";
        let args = build_args(evil, None);
        assert_eq!(args, vec![evil.to_string()]);
    }

    #[test]
    fn parse_stdout_filters_blank_lines_and_trims() {
        let out = "/a/b.pdf\n  /c/d.txt  \n\n/e/f.md\n";
        let paths = parse_stdout(out);
        assert_eq!(paths, vec!["/a/b.pdf", "/c/d.txt", "/e/f.md"]);
    }

    #[test]
    fn parse_stdout_empty_is_empty() {
        assert!(parse_stdout("").is_empty());
        assert!(parse_stdout("\n\n").is_empty());
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let r = spotlight_search("   ", None, None).await;
        assert!(matches!(r, Err(SpotlightError::EmptyQuery)));
    }

    #[tokio::test]
    async fn nonexistent_scope_dir_is_rejected() {
        #[cfg(target_os = "macos")]
        {
            let r = spotlight_search(
                "x",
                Some(Path::new("/definitely/not/here/duduclaw-os-xyz")),
                None,
            )
            .await;
            assert!(matches!(r, Err(SpotlightError::ScopeNotFound(_))));
        }
    }

    #[test]
    fn limit_clamping_bounds() {
        // Documents the clamp behavior used inline in `spotlight_search`
        // (kept as a standalone assertion since the async path requires a
        // live `mdfind`/macOS to exercise end-to-end). `clamp_limit` mirrors
        // the exact expression `spotlight_search` applies to its `limit` arg.
        fn clamp_limit(requested: Option<usize>) -> usize {
            requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
        }
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
    }
}
