//! Constants shared by `duduclaw-maint-shell` and `duduclaw-maint-helper`,
//! and the closed token set spoken over the helper socket.
//!
//! The wire protocol is one line from the client: exactly one of the six
//! tokens below followed by `\n`. Nothing else is parsed.
//! - `gateway`, `gateway-follow`, `model`, `model-follow`: the helper answers
//!   with plain-text log lines and closes the connection when done (or, for
//!   the `-follow` tokens, when the client hangs up).
//! - `restart-gateway`, `restart-model`: the helper restarts that one fixed
//!   unit and answers with a single status line starting `ok:` or `error:`.
//!
//! The restarts go through this root helper rather than polkit (owner
//! decision 2026-10-16: adding polkit to the base edition can renumber
//! existing system accounts).

/// uid (and gid) of the `duduclaw-maint` account. Fixed by the OS recipe
/// (`useradd --system -u 4800`), chosen to stay clear of the host's dynamic
/// system range, the gateway's subuid range and Android AIDs inside Waydroid.
/// `duduclaw-maint-helper` admits only this uid.
pub const MAINT_UID: u32 = 4800;

/// uid the gateway and the local model server run as (`duduclaw`). The log
/// service only returns journal records whose `_UID` is this value.
pub const SERVICE_UID: u32 = 1000;

/// Unix socket `duduclaw-maint-helper.socket` listens on.
pub const HELPER_SOCKET_PATH: &str = "/run/duduclaw-maint-helper.sock";

/// Directory (systemd `RuntimeDirectory=duduclaw-maint-helper`, preserved
/// across instances) where the helper keeps its per-unit restart stamps.
pub const HELPER_STATE_DIR: &str = "/run/duduclaw-maint-helper";

/// Minimum time between two restarts of the same unit through the helper.
pub const RESTART_MIN_INTERVAL_SECS: u64 = 60;

/// systemd unit of the gateway.
pub const GATEWAY_UNIT: &str = "duduclaw-gateway.service";

/// systemd unit of the local model server.
pub const MODEL_UNIT: &str = "duduclaw-llama-server.service";

/// Longest request line (without the newline) the server reads. The longest
/// token is 15 bytes; anything past this is rejected without buffering it.
pub const MAX_TOKEN_LINE_BYTES: usize = 32;

/// Which service's journal to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    Gateway,
    Model,
}

impl LogSource {
    /// systemd unit the journal query is filtered on.
    pub fn unit(self) -> &'static str {
        match self {
            LogSource::Gateway => GATEWAY_UNIT,
            LogSource::Model => MODEL_UNIT,
        }
    }
}

/// One log request: a source, and whether to keep following new records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRequest {
    pub source: LogSource,
    pub follow: bool,
}

/// Which unit a restart request targets. Only these two exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartTarget {
    Gateway,
    Model,
}

impl RestartTarget {
    pub const ALL: [RestartTarget; 2] = [RestartTarget::Gateway, RestartTarget::Model];

    /// The wire token.
    pub fn token(self) -> &'static str {
        match self {
            RestartTarget::Gateway => "restart-gateway",
            RestartTarget::Model => "restart-model",
        }
    }

    /// The one unit this target restarts.
    pub fn unit(self) -> &'static str {
        match self {
            RestartTarget::Gateway => GATEWAY_UNIT,
            RestartTarget::Model => MODEL_UNIT,
        }
    }

    /// Short name used for the helper's restart stamp file.
    pub fn key(self) -> &'static str {
        match self {
            RestartTarget::Gateway => "gateway",
            RestartTarget::Model => "model",
        }
    }

    /// The fixed `systemctl` arguments (after the program path). Run by the
    /// root helper, so no polkit is involved.
    pub fn systemctl_args(self) -> [&'static str; 4] {
        ["--no-pager", "--no-ask-password", "restart", self.unit()]
    }
}

/// Everything the helper accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperRequest {
    Logs(LogRequest),
    Restart(RestartTarget),
}

impl HelperRequest {
    /// The wire token.
    pub fn token(self) -> &'static str {
        match self {
            HelperRequest::Logs(r) => r.token(),
            HelperRequest::Restart(t) => t.token(),
        }
    }

    /// Parse one request line (the bytes before `\n`): exact, byte-for-byte
    /// match against the six tokens; no trimming, no case folding.
    pub fn parse_token(line: &[u8]) -> Result<HelperRequest, TokenError> {
        match LogRequest::parse_token(line) {
            Ok(r) => Ok(HelperRequest::Logs(r)),
            Err(TokenError::Unknown) => RestartTarget::ALL
                .into_iter()
                .find(|t| t.token().as_bytes() == line)
                .map(HelperRequest::Restart)
                .ok_or(TokenError::Unknown),
            Err(e) => Err(e),
        }
    }
}

/// Reply line prefix for a successful restart.
pub const REPLY_OK_PREFIX: &str = "ok:";

/// Why a request line was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Empty,
    TooLong,
    Unknown,
}

impl LogRequest {
    /// Every request the service understands, in menu order.
    pub const ALL: [LogRequest; 4] = [
        LogRequest {
            source: LogSource::Gateway,
            follow: false,
        },
        LogRequest {
            source: LogSource::Gateway,
            follow: true,
        },
        LogRequest {
            source: LogSource::Model,
            follow: false,
        },
        LogRequest {
            source: LogSource::Model,
            follow: true,
        },
    ];

    /// The wire token for this request.
    pub fn token(self) -> &'static str {
        match (self.source, self.follow) {
            (LogSource::Gateway, false) => "gateway",
            (LogSource::Gateway, true) => "gateway-follow",
            (LogSource::Model, false) => "model",
            (LogSource::Model, true) => "model-follow",
        }
    }

    /// Parse one request line (the bytes before `\n`). Exact, byte-for-byte
    /// match against the closed token set: no trimming, no case folding, no
    /// trailing `\r`. Anything else is refused.
    pub fn parse_token(line: &[u8]) -> Result<LogRequest, TokenError> {
        if line.is_empty() {
            return Err(TokenError::Empty);
        }
        if line.len() > MAX_TOKEN_LINE_BYTES {
            return Err(TokenError::TooLong);
        }
        LogRequest::ALL
            .into_iter()
            .find(|r| r.token().as_bytes() == line)
            .ok_or(TokenError::Unknown)
    }

    /// The fixed `journalctl` arguments (after the program path) for this
    /// request. Nothing in here comes from the client: the token only picks
    /// one of four constant shapes.
    ///
    /// `-o json` so the filter can look at journald's own `MESSAGE` field
    /// instead of a pre-formatted line; `--output-fields=MESSAGE` keeps every
    /// other field (command lines, cgroup paths) out of the pipe altogether
    /// (journald still adds `__REALTIME_TIMESTAMP` and the cursor fields).
    /// `_SYSTEMD_UNIT=` and `_UID=` are trusted fields filled in by journald,
    /// so a process cannot forge its way into this view.
    pub fn journalctl_args(self) -> Vec<String> {
        let mut args: Vec<String> = [
            "--no-pager",
            "-o",
            "json",
            "--output-fields=MESSAGE",
            "-n",
            "1000",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if self.follow {
            args.push("-f".to_string());
        }
        args.push(format!("_SYSTEMD_UNIT={}", self.source.unit()));
        args.push(format!("_UID={SERVICE_UID}"));
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_round_trips() {
        for r in LogRequest::ALL {
            assert_eq!(LogRequest::parse_token(r.token().as_bytes()), Ok(r));
        }
    }

    #[test]
    fn near_miss_tokens_are_refused() {
        for bad in [
            &b"Gateway"[..],
            b"gateway ",
            b" gateway",
            b"gateway\r",
            b"gateway\n",
            b"gateway\0",
            b"gateway-follow2",
            b"gateway;id",
            b"sysd",
            b"kernel",
            b"all",
            b"../gateway",
            b"_SYSTEMD_UNIT=dropbear.service",
            b"model --all",
        ] {
            assert!(
                LogRequest::parse_token(bad).is_err(),
                "must refuse {:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(LogRequest::parse_token(b""), Err(TokenError::Empty));
        assert_eq!(
            LogRequest::parse_token(&[b'a'; MAX_TOKEN_LINE_BYTES + 1]),
            Err(TokenError::TooLong)
        );
    }

    #[test]
    fn helper_tokens_round_trip_and_nothing_else_parses() {
        for r in LogRequest::ALL {
            assert_eq!(
                HelperRequest::parse_token(r.token().as_bytes()),
                Ok(HelperRequest::Logs(r))
            );
        }
        for t in RestartTarget::ALL {
            assert_eq!(
                HelperRequest::parse_token(t.token().as_bytes()),
                Ok(HelperRequest::Restart(t))
            );
            assert!(t.token().len() <= MAX_TOKEN_LINE_BYTES);
        }
        for bad in [
            &b"restart"[..],
            b"restart-sysd",
            b"restart-shell",
            b"restart-dropbear",
            b"restart-gateway ",
            b"restart-gateway\r",
            b"Restart-model",
            b"restart-model;reboot",
            b"restart duduclaw-gateway.service",
            b"reboot",
            b"stop-gateway",
        ] {
            assert_eq!(
                HelperRequest::parse_token(bad),
                Err(TokenError::Unknown),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn restart_args_are_fixed() {
        assert_eq!(
            RestartTarget::Gateway.systemctl_args(),
            [
                "--no-pager",
                "--no-ask-password",
                "restart",
                "duduclaw-gateway.service"
            ]
        );
        assert_eq!(
            RestartTarget::Model.systemctl_args(),
            [
                "--no-pager",
                "--no-ask-password",
                "restart",
                "duduclaw-llama-server.service"
            ]
        );
    }

    #[test]
    fn journalctl_args_are_fixed_per_token() {
        let g = LogRequest {
            source: LogSource::Gateway,
            follow: false,
        }
        .journalctl_args();
        assert_eq!(
            g,
            [
                "--no-pager",
                "-o",
                "json",
                "--output-fields=MESSAGE",
                "-n",
                "1000",
                "_SYSTEMD_UNIT=duduclaw-gateway.service",
                "_UID=1000"
            ]
        );
        let mf = LogRequest {
            source: LogSource::Model,
            follow: true,
        }
        .journalctl_args();
        assert!(mf.contains(&"-f".to_string()));
        assert!(mf.contains(&"_SYSTEMD_UNIT=duduclaw-llama-server.service".to_string()));
        assert!(mf.contains(&"_UID=1000".to_string()));
        // Only the follow variants follow.
        assert!(!g.contains(&"-f".to_string()));
    }
}
