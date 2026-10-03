//! One answer to "is Docker usable right now?" shared by every surface that
//! reports or depends on it (`duduclaw doctor`, the task sandbox, computer
//! use, the script sandbox).
//!
//! `GET /_ping` is not enough: on 2026-10-03 a half-dead Docker Desktop
//! answered the ping while `/info` and `docker ps` returned EOF, so `doctor`
//! printed "Docker daemon is reachable" next to sandbox rows saying the
//! opposite. The probe therefore asks for the daemon's `info` (server
//! version) **and** a cheap container list, each under a short timeout, and
//! treats an error, EOF, empty answer or timeout as unavailable.
//!
//! This module holds only the classification (pure, unit-tested). The
//! transports run the requests: the docker CLI (`docker info --format
//! {{.ServerVersion}}` + `docker ps --quiet --last 1`, see
//! [`INFO_ARGS`] / [`LIST_ARGS`]) in the gateway, the Docker API (`info` +
//! `list_containers`) in `duduclaw-container`.

use std::time::Duration;

/// Upper bound for each of the two requests.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// docker CLI arguments of the `info` request.
pub const INFO_ARGS: [&str; 3] = ["info", "--format", "{{.ServerVersion}}"];
/// docker CLI arguments of the cheap list request. `--last 1` (alias `-n`):
/// `docker ps` has no `--limit` flag (docker CLI 29 answers `unknown flag:
/// --limit`, exit 125, which made every surface report a healthy daemon as
/// unusable). `--last` is accepted by every Docker CLI version in use.
pub const LIST_ARGS: [&str; 4] = ["ps", "--quiet", "--last", "1"];

/// What one request produced. `None` at the call site means the request
/// could not be made at all (no client binary, spawn failure).
#[derive(Debug, Clone, Copy)]
pub struct Answer<'a> {
    /// Exit status / HTTP result was a success.
    pub success: bool,
    /// The request hit [`PROBE_TIMEOUT`] (or the caller's bound).
    pub timed_out: bool,
    /// Standard output (CLI) or the extracted field (API).
    pub stdout: &'a str,
    /// Standard error (CLI) or the error text (API); `""` when none.
    pub stderr: &'a str,
}

/// First non-empty stderr line of the request that made Docker unavailable
/// (`info` when it failed, otherwise the list), bounded to 200 characters,
/// so a doctor row can say *why* (e.g. a rejected CLI flag).
pub fn failure_detail(info: Option<Answer<'_>>, list: Option<Answer<'_>>) -> Option<String> {
    let failing = match classify_info(info) {
        Err(_) => info,
        Ok(_) => list.filter(|l| !l.success || l.timed_out),
    }?;
    let line = failing.stderr.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(crate::truncate_chars(line, 200).to_string())
}

/// Why Docker counts as unavailable. `code()` is stable for logs/audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The request could not be made (no docker client, spawn error).
    NoClient,
    /// `info` or the list did not answer in time.
    Timeout,
    /// `info` failed (connection refused, EOF, daemon error).
    InfoFailed,
    /// `info` "succeeded" but carried no usable server version.
    EmptyInfo,
    /// `info` answered but listing containers failed.
    ListFailed,
}

impl Unavailable {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoClient => "no_client",
            Self::Timeout => "timeout",
            Self::InfoFailed => "info_failed",
            Self::EmptyInfo => "empty_info",
            Self::ListFailed => "list_failed",
        }
    }
}

/// The verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerStatus {
    Reachable { server_version: String },
    Unavailable(Unavailable),
}

impl DockerStatus {
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable { .. })
    }
}

/// A server version as Docker prints it (`27.3.1`, `28.0.0-rc.1`,
/// `20.10.24+dfsg1`). Anything else — empty, `<no value>`, an error line
/// that slipped onto stdout — is not a version.
pub fn server_version(raw: &str) -> Option<String> {
    let v = raw.trim();
    let ok = !v.is_empty()
        && v.len() <= 64
        && v.as_bytes()[0].is_ascii_digit()
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_' | b'~'));
    ok.then(|| v.to_string())
}

/// Classify the `info` answer alone. `Ok(version)` means the list request
/// should be made next.
pub fn classify_info(info: Option<Answer<'_>>) -> Result<String, Unavailable> {
    let info = info.ok_or(Unavailable::NoClient)?;
    if info.timed_out {
        return Err(Unavailable::Timeout);
    }
    if !info.success {
        return Err(Unavailable::InfoFailed);
    }
    server_version(info.stdout).ok_or(Unavailable::EmptyInfo)
}

/// Classify both answers. `list` is consulted only when `info` passed; its
/// output may be empty (no containers), but it must have succeeded.
pub fn classify(info: Option<Answer<'_>>, list: Option<Answer<'_>>) -> DockerStatus {
    let server_version = match classify_info(info) {
        Ok(v) => v,
        Err(why) => return DockerStatus::Unavailable(why),
    };
    match list {
        None => DockerStatus::Unavailable(Unavailable::NoClient),
        Some(l) if l.timed_out => DockerStatus::Unavailable(Unavailable::Timeout),
        Some(l) if !l.success => DockerStatus::Unavailable(Unavailable::ListFailed),
        Some(_) => DockerStatus::Reachable { server_version },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> Option<Answer<'_>> {
        Some(Answer { success: true, timed_out: false, stdout, stderr: "" })
    }
    fn failed() -> Option<Answer<'static>> {
        Some(Answer { success: false, timed_out: false, stdout: "", stderr: "" })
    }
    fn timeout() -> Option<Answer<'static>> {
        Some(Answer { success: false, timed_out: true, stdout: "", stderr: "" })
    }

    #[test]
    fn healthy_daemon_is_reachable() {
        assert_eq!(
            classify(ok("27.3.1\n"), ok("")),
            DockerStatus::Reachable { server_version: "27.3.1".into() }
        );
        assert!(classify(ok("28.0.0-rc.1"), ok("abc123\n")).is_reachable());
    }

    #[test]
    fn half_dead_daemon_ping_ok_info_eof_is_unavailable() {
        // The 2026-10-03 shape: `/_ping` answered, `/info` returned EOF.
        assert_eq!(classify(failed(), None), DockerStatus::Unavailable(Unavailable::InfoFailed));
    }

    #[test]
    fn info_ok_but_list_eof_is_unavailable() {
        assert_eq!(classify(ok("27.3.1"), failed()), DockerStatus::Unavailable(Unavailable::ListFailed));
    }

    #[test]
    fn empty_or_placeholder_info_is_unavailable() {
        for raw in ["", "\n", "<no value>", "error during connect: EOF", "Cannot connect"] {
            assert_eq!(
                classify(ok(raw), ok("")),
                DockerStatus::Unavailable(Unavailable::EmptyInfo),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn timeouts_and_missing_client_are_unavailable() {
        assert_eq!(classify(timeout(), None), DockerStatus::Unavailable(Unavailable::Timeout));
        assert_eq!(classify(ok("27.0.1"), timeout()), DockerStatus::Unavailable(Unavailable::Timeout));
        assert_eq!(classify(None, None), DockerStatus::Unavailable(Unavailable::NoClient));
        assert_eq!(classify(ok("27.0.1"), None), DockerStatus::Unavailable(Unavailable::NoClient));
    }

    #[test]
    fn versions_with_suffixes_parse() {
        assert_eq!(server_version(" 20.10.24+dfsg1 ").as_deref(), Some("20.10.24+dfsg1"));
        assert_eq!(server_version("v27"), None);
        assert_eq!(server_version(&"9".repeat(65)), None);
    }

    #[test]
    fn list_argv_is_exactly_ps_quiet_last_1() {
        // `--limit` does not exist on `docker ps`; this argv must stay valid
        // for the installed CLIs (checked: `docker ps --quiet --last 1` exits 0).
        assert_eq!(LIST_ARGS, ["ps", "--quiet", "--last", "1"]);
        assert_eq!(INFO_ARGS, ["info", "--format", "{{.ServerVersion}}"]);
    }

    #[test]
    fn a_rejected_list_flag_is_list_failed_with_its_stderr() {
        let info = ok("29.0.1");
        let list = Some(Answer {
            success: false,
            timed_out: false,
            stdout: "",
            stderr: "unknown flag: --limit\nSee 'docker ps --help'.\n",
        });
        assert_eq!(classify(info, list), DockerStatus::Unavailable(Unavailable::ListFailed));
        assert_eq!(failure_detail(info, list).as_deref(), Some("unknown flag: --limit"));
        assert_eq!(failure_detail(info, ok("")), None);
    }
}
