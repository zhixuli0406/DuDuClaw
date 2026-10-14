//! Container-engine facts that sit next to the availability probe
//! ([`crate::docker_probe`]): which endpoint a container client talks to, and
//! whether the engine actually enforces the resource limits the container
//! features rely on.
//!
//! Two pieces, both shared by the gateway (docker CLI transport) and
//! `duduclaw-container` (Docker API transport):
//!
//! 1. **Endpoint pass-through.** Several container clients run with a cleared
//!    environment (`env_clear`) so no gateway secret reaches them. On DuDuClaw
//!    OS the gateway reaches its rootless Podman service through
//!    `DOCKER_HOST` / `CONTAINER_HOST` (remote mode), so those clients must
//!    carry exactly those two names from the gateway's own environment.
//!    [`endpoint_env_pairs`] returns them when set; nothing changes when they
//!    are unset. They are NOT on the agent CLI allowlist
//!    (`spawn_env::AGENT_CLI_ENV_ALLOWLIST`): an AI CLI never needs the
//!    container endpoint.
//!
//! 2. **Resource-limit self-check (fail closed).** A rootless engine whose
//!    cgroup controllers were not delegated accepts `--memory` / `--pids-limit`
//!    and silently ignores them (measured: Podman with `Delegate=yes` alone).
//!    A sandbox running without its ceilings must not report available. On a
//!    rootless engine — and only there, so rootful Docker is unaffected — a
//!    tiny probe container with a memory and pids limit reads its own
//!    `memory.max` / `pids.max`; `max` (or anything but the requested limit)
//!    marks the features unavailable. This module holds the decision logic
//!    (pure, unit-tested) and a per-process memo; the transports run the
//!    requests.

use std::ffi::OsString;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Environment names that select the container engine endpoint: Docker's
/// CLI and API clients read `DOCKER_HOST`, Podman's CLI reads
/// `CONTAINER_HOST` (and switches to remote mode).
pub const ENDPOINT_ENV_VARS: [&str; 2] = ["DOCKER_HOST", "CONTAINER_HOST"];

/// [`ENDPOINT_ENV_VARS`] that are set (non-empty) in this process's
/// environment, for a container client that runs with a cleared environment.
pub fn endpoint_env_pairs() -> Vec<(&'static str, OsString)> {
    endpoint_env_pairs_from(|name| std::env::var_os(name))
}

/// [`endpoint_env_pairs`] over an injected lookup (unit-testable). Empty
/// values are dropped: an empty `DOCKER_HOST` is not an endpoint.
pub fn endpoint_env_pairs_from(lookup: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, OsString)> {
    ENDPOINT_ENV_VARS
        .iter()
        .filter_map(|name| lookup(name).filter(|v| !v.is_empty()).map(|v| (*name, v)))
        .collect()
}

// ---------------------------------------------------------------------------
// Rootless detection
// ---------------------------------------------------------------------------

/// docker / podman CLI arguments that print the engine's `info` as JSON.
/// Both CLIs accept the `{{json .}}` template (Docker prints its
/// `SecurityOptions`, Podman its `host.security.rootless`).
pub const ROOTLESS_INFO_ARGS: [&str; 3] = ["info", "--format", "{{json .}}"];

/// Does a Docker-API `SecurityOptions` list name `rootless`? Each entry is a
/// comma-separated `key=value` list (`name=seccomp,profile=builtin`); the
/// match is on a whole `name=rootless` field, never a substring.
pub fn security_options_report_rootless<S: AsRef<str>>(options: &[S]) -> bool {
    options
        .iter()
        .any(|entry| entry.as_ref().split(',').any(|field| field.trim() == "name=rootless"))
}

/// Read the `info` JSON of either engine. `Some(true)`: the engine runs
/// rootless; `Some(false)`: the answer is recognisable and does not say
/// rootless; `None`: unparseable or neither shape present (the caller then
/// runs the limit probe anyway — unknown never counts as rootful).
pub fn info_reports_rootless(json: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    // Podman (`podman info`): host.security.rootless.
    let podman = value
        .get("host")
        .and_then(|h| h.get("security"))
        .and_then(|s| s.get("rootless"))
        .and_then(serde_json::Value::as_bool);
    // Docker (and Podman's compat API): SecurityOptions.
    let docker = value.get("SecurityOptions").and_then(serde_json::Value::as_array).map(|options| {
        let strings: Vec<&str> = options.iter().filter_map(serde_json::Value::as_str).collect();
        security_options_report_rootless(&strings)
    });
    match (podman, docker) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (None, None) => None,
        _ => Some(false),
    }
}

// ---------------------------------------------------------------------------
// Resource-limit probe
// ---------------------------------------------------------------------------

/// Memory limit the probe container asks for (32 MiB, page-aligned).
pub const PROBE_MEMORY_BYTES: u64 = 32 * 1024 * 1024;
/// Process limit the probe container asks for.
pub const PROBE_PIDS_LIMIT: u64 = 32;
/// Files the probe prints, in this order (cgroup v2, read inside the
/// container's own cgroup namespace).
pub const LIMIT_FILES: [&str; 2] = ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/pids.max"];
/// Upper bound of one probe run (container start on a slow device included).
pub const LIMIT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Name prefix of the probe container (so a leftover is recognisable).
pub const LIMIT_PROBE_NAME_PREFIX: &str = "duduclaw-limit-probe-";

/// docker / podman CLI argv of the probe: a throw-away container from
/// `image` (already required by the feature, never pulled) with no network,
/// a read-only root, no capabilities and the probe limits, printing
/// [`LIMIT_FILES`]. `None` when `image` is not a valid reference. Every
/// element except the name and the image is a literal.
pub fn limit_probe_args(image: &str, name: &str) -> Option<Vec<String>> {
    if !crate::sandbox_image::valid_image(image) || !valid_probe_name(name) {
        return None;
    }
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "none",
        "--read-only",
        "--user",
        "65534:65534",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--memory",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(PROBE_MEMORY_BYTES.to_string());
    args.push("--memory-swap".into());
    args.push(PROBE_MEMORY_BYTES.to_string());
    args.push("--pids-limit".into());
    args.push(PROBE_PIDS_LIMIT.to_string());
    args.extend(["--entrypoint", "cat", image].iter().map(|s| s.to_string()));
    args.extend(LIMIT_FILES.iter().map(|s| s.to_string()));
    Some(args)
}

/// A fresh probe container name: [`LIMIT_PROBE_NAME_PREFIX`] + 32 hex digits.
pub fn new_probe_name() -> String {
    format!("{LIMIT_PROBE_NAME_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

fn valid_probe_name(name: &str) -> bool {
    name.strip_prefix(LIMIT_PROBE_NAME_PREFIX)
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What the probe showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitVerdict {
    /// Both files carry a limit no larger than requested.
    Enforced,
    /// A file reads `max`: the engine accepted the limit and ignored it.
    Ignored,
    /// The files carry something else (larger than requested, malformed).
    Unexpected,
    /// The probe did not run to completion (no `cat` in the image, missing
    /// cgroup file, timeout, engine error).
    ProbeFailed,
}

impl LimitVerdict {
    /// Stable code for logs and audit.
    pub fn code(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::Ignored => "limits_ignored",
            Self::Unexpected => "limits_unexpected",
            Self::ProbeFailed => "limit_probe_failed",
        }
    }

    pub fn is_enforced(self) -> bool {
        self == Self::Enforced
    }

    /// A verdict about the engine itself (kept for the life of the process)
    /// rather than about one attempt (retried after [`RETRY_AFTER`]).
    pub fn is_definitive(self) -> bool {
        matches!(self, Self::Enforced | Self::Ignored)
    }

    /// Operator-facing reason (zh-TW) when not enforced.
    pub fn reason_zh(self) -> &'static str {
        match self {
            Self::Enforced => "容器資源上限有效",
            Self::Ignored => "rootless 容器引擎沒有套用記憶體／程序數上限（cgroup 控制器未委派），\
                              容器功能停用，以免在沒有上限的情況下執行",
            Self::Unexpected => "rootless 容器引擎回報的資源上限與要求不符，容器功能停用",
            Self::ProbeFailed => "無法確認 rootless 容器引擎是否套用資源上限（檢查容器沒有跑完），容器功能停用",
        }
    }

    /// Operator-facing reason (English) when not enforced.
    pub fn reason_en(self) -> &'static str {
        match self {
            Self::Enforced => "container resource limits are enforced",
            Self::Ignored => "the rootless container engine ignores memory/pids limits (cgroup controllers \
                              not delegated); container features are off rather than running unlimited",
            Self::Unexpected => "the rootless container engine reports limits other than requested; \
                                 container features are off",
            Self::ProbeFailed => "could not confirm that the rootless container engine enforces resource limits \
                                  (the check container did not finish); container features are off",
        }
    }
}

/// Classify the probe's outcome. `stdout` is `memory.max` then `pids.max`,
/// one per line.
pub fn classify_limit_output(success: bool, timed_out: bool, stdout: &str) -> LimitVerdict {
    if timed_out || !success {
        return LimitVerdict::ProbeFailed;
    }
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let [memory, pids] = lines.as_slice() else {
        return LimitVerdict::ProbeFailed;
    };
    if *memory == "max" || *pids == "max" {
        return LimitVerdict::Ignored;
    }
    let within = |raw: &str, limit: u64| raw.parse::<u64>().is_ok_and(|v| v > 0 && v <= limit);
    if within(memory, PROBE_MEMORY_BYTES) && within(pids, PROBE_PIDS_LIMIT) {
        LimitVerdict::Enforced
    } else {
        LimitVerdict::Unexpected
    }
}

/// The whole decision: `rootless` from [`info_reports_rootless`] (or the API
/// security options), `probe` the probe's verdict when it ran. A rootful
/// engine (`Some(false)`) is never probed and always passes; a rootless or
/// unknown engine passes only on [`LimitVerdict::Enforced`].
pub fn decide(rootless: Option<bool>, probe: impl FnOnce() -> LimitVerdict) -> LimitVerdict {
    match rootless {
        Some(false) => LimitVerdict::Enforced,
        _ => probe(),
    }
}

/// How long a non-definitive verdict is reused before the probe runs again.
pub const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Per-process memo of the verdict: a definitive one (see
/// [`LimitVerdict::is_definitive`]) is kept for the life of the process, a
/// failed attempt for [`RETRY_AFTER`].
pub struct LimitMemo {
    slot: Mutex<Option<(Instant, LimitVerdict)>>,
}

impl LimitMemo {
    pub const fn new() -> Self {
        Self { slot: Mutex::new(None) }
    }

    /// The remembered verdict, if still valid at `now`.
    pub fn get_at(&self, now: Instant) -> Option<LimitVerdict> {
        let slot = self.slot.lock().ok()?;
        let (at, verdict) = (*slot)?;
        (verdict.is_definitive() || now.saturating_duration_since(at) < RETRY_AFTER).then_some(verdict)
    }

    pub fn get(&self) -> Option<LimitVerdict> {
        self.get_at(Instant::now())
    }

    pub fn put_at(&self, verdict: LimitVerdict, now: Instant) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some((now, verdict));
        }
    }

    pub fn put(&self, verdict: LimitVerdict) {
        self.put_at(verdict, Instant::now());
    }
}

impl Default for LimitMemo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_pairs_copy_only_the_two_names_when_set() {
        let pairs = endpoint_env_pairs_from(|name| match name {
            "DOCKER_HOST" => Some("unix:///run/duduclaw-gateway/podman/podman.sock".into()),
            "CONTAINER_HOST" => Some("unix:///run/duduclaw-gateway/podman/podman.sock".into()),
            _ => Some("leak".into()),
        });
        let names: Vec<&str> = pairs.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["DOCKER_HOST", "CONTAINER_HOST"]);
        assert!(endpoint_env_pairs_from(|_| None).is_empty(), "unset ⇒ nothing added");
        assert!(endpoint_env_pairs_from(|_| Some(OsString::new())).is_empty(), "empty is not an endpoint");
        let only_podman = endpoint_env_pairs_from(|n| (n == "CONTAINER_HOST").then(|| "unix:///x".into()));
        assert_eq!(only_podman, vec![("CONTAINER_HOST", OsString::from("unix:///x"))]);
    }

    #[test]
    fn endpoint_names_are_not_on_the_agent_cli_allowlist() {
        for name in ENDPOINT_ENV_VARS {
            assert!(!crate::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&name), "{name}");
        }
    }

    #[test]
    fn docker_info_security_options_are_matched_by_field() {
        let rootless = r#"{"ServerVersion":"27.3.1","SecurityOptions":["name=seccomp,profile=builtin","name=rootless","name=cgroupns"]}"#;
        assert_eq!(info_reports_rootless(rootless), Some(true));
        let rootful = r#"{"ServerVersion":"27.3.1","SecurityOptions":["name=apparmor","name=seccomp,profile=builtin","name=cgroupns"]}"#;
        assert_eq!(info_reports_rootless(rootful), Some(false));
        // A substring is not the field.
        assert!(!security_options_report_rootless(&["name=rootlessish", "profile=name=rootless-x"]));
        assert!(security_options_report_rootless(&["name=seccomp,name=rootless"]));
    }

    #[test]
    fn podman_info_host_security_rootless_is_read() {
        let rootless = r#"{"host":{"security":{"rootless":true,"seccompEnabled":true}},"version":{"Version":"5.8.7"}}"#;
        assert_eq!(info_reports_rootless(rootless), Some(true));
        let rootful = r#"{"host":{"security":{"rootless":false}}}"#;
        assert_eq!(info_reports_rootless(rootful), Some(false));
    }

    #[test]
    fn unrecognisable_info_is_unknown_not_rootful() {
        for raw in ["", "27.3.1", "{}", r#"{"host":{}}"#, "not json", r#"{"SecurityOptions":"name=rootless"}"#] {
            assert_eq!(info_reports_rootless(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn probe_argv_is_literal_with_limits_and_no_network() {
        let name = format!("{LIMIT_PROBE_NAME_PREFIX}{}", "a".repeat(32));
        let args = limit_probe_args("ghcr.io/zhixuli0406/duduclaw:v1.73.2", &name).unwrap();
        let joined = args.join(" ");
        assert_eq!(
            joined,
            format!(
                "run --rm --pull never --name {name} --network none --read-only --user 65534:65534 \
                 --cap-drop ALL --security-opt no-new-privileges:true --memory 33554432 --memory-swap 33554432 \
                 --pids-limit 32 --entrypoint cat ghcr.io/zhixuli0406/duduclaw:v1.73.2 \
                 /sys/fs/cgroup/memory.max /sys/fs/cgroup/pids.max"
            )
        );
        assert!(limit_probe_args("--privileged", &name).is_none(), "option-like image refused");
        assert!(limit_probe_args("img", "attacker-name").is_none(), "only our own names");
        assert!(valid_probe_name(&new_probe_name()));
    }

    #[test]
    fn limits_reading_max_are_ignored() {
        assert_eq!(classify_limit_output(true, false, "max\nmax\n"), LimitVerdict::Ignored);
        assert_eq!(classify_limit_output(true, false, "max\n32\n"), LimitVerdict::Ignored);
        assert_eq!(classify_limit_output(true, false, "33554432\nmax\n"), LimitVerdict::Ignored);
    }

    #[test]
    fn requested_limits_are_enforced() {
        assert_eq!(classify_limit_output(true, false, "33554432\n32\n"), LimitVerdict::Enforced);
        // A tighter value (page rounding) still counts.
        assert_eq!(classify_limit_output(true, false, " 33550336 \n 16 \n"), LimitVerdict::Enforced);
    }

    #[test]
    fn other_values_and_failures_fail_closed() {
        assert_eq!(classify_limit_output(true, false, "67108864\n32\n"), LimitVerdict::Unexpected);
        assert_eq!(classify_limit_output(true, false, "33554432\n4096\n"), LimitVerdict::Unexpected);
        assert_eq!(classify_limit_output(true, false, "0\n32\n"), LimitVerdict::Unexpected);
        assert_eq!(classify_limit_output(true, false, "garbage\n32\n"), LimitVerdict::Unexpected);
        assert_eq!(classify_limit_output(true, false, "33554432\n"), LimitVerdict::ProbeFailed);
        assert_eq!(classify_limit_output(true, false, ""), LimitVerdict::ProbeFailed);
        assert_eq!(classify_limit_output(true, false, "1\n2\n3\n"), LimitVerdict::ProbeFailed);
        assert_eq!(classify_limit_output(false, false, "33554432\n32\n"), LimitVerdict::ProbeFailed);
        assert_eq!(classify_limit_output(true, true, "33554432\n32\n"), LimitVerdict::ProbeFailed);
    }

    #[test]
    fn rootful_engines_are_never_probed_and_unknown_ones_are() {
        let mut ran = false;
        assert_eq!(decide(Some(false), || { ran = true; LimitVerdict::Ignored }), LimitVerdict::Enforced);
        assert!(!ran, "rootful Docker is unaffected");
        assert_eq!(decide(Some(true), || LimitVerdict::Ignored), LimitVerdict::Ignored);
        assert_eq!(decide(None, || LimitVerdict::ProbeFailed), LimitVerdict::ProbeFailed);
        assert_eq!(decide(Some(true), || LimitVerdict::Enforced), LimitVerdict::Enforced);
    }

    #[test]
    fn memo_keeps_definitive_verdicts_and_retries_failures() {
        let t0 = Instant::now();
        let memo = LimitMemo::new();
        assert_eq!(memo.get_at(t0), None);
        memo.put_at(LimitVerdict::ProbeFailed, t0);
        assert_eq!(memo.get_at(t0 + Duration::from_secs(1)), Some(LimitVerdict::ProbeFailed));
        assert_eq!(memo.get_at(t0 + RETRY_AFTER), None, "a failed attempt is retried");
        memo.put_at(LimitVerdict::Ignored, t0);
        assert_eq!(memo.get_at(t0 + Duration::from_secs(86_400)), Some(LimitVerdict::Ignored));
        memo.put_at(LimitVerdict::Enforced, t0);
        assert_eq!(memo.get_at(t0 + Duration::from_secs(86_400)), Some(LimitVerdict::Enforced));
    }

    #[test]
    fn every_unenforced_verdict_has_a_reason() {
        for v in [LimitVerdict::Ignored, LimitVerdict::Unexpected, LimitVerdict::ProbeFailed] {
            assert!(!v.is_enforced());
            assert!(v.reason_zh().contains("rootless"), "{v:?}");
            assert!(v.reason_en().contains("rootless"), "{v:?}");
        }
    }
}
