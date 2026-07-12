//! G3 event-triggered cron — condition evaluation + trigger-kind state machine.
//!
//! Extends the time-only cron scheduler with two event-driven trigger kinds:
//!
//! - **`condition`** — at each due cron slot, run a headless *condition script*
//!   in a native OS sandbox ([`duduclaw_sandbox`]). The script receives the
//!   prior persisted state via the `DUDUCLAW_CRON_STATE` env var and must print
//!   `{ "fire": bool, "message"?: string, "state"?: object }` JSON to stdout.
//!   The task fires only when `fire == true`; `state` (≤16 KiB) is persisted for
//!   the next evaluation; `message` is injected into the fired prompt.
//! - **`on_exit`** — at each due slot, run a *watch command* in the sandbox; the
//!   task fires only when that command exits with status 0.
//!
//! # Fail-closed (project invariant I5)
//! Every failure mode — sandbox refusal, spawn error, timeout, non-JSON output,
//! missing `fire`, oversized state — resolves to **do not fire** and logs.
//! Never does a failure fall through to firing the task.
//!
//! # Testable seam
//! The security-sensitive decision logic ([`parse_condition_output`],
//! [`interpret_condition_exec`], [`interpret_on_exit_exec`], [`TriggerKind`],
//! [`plan_condition_command`]) is pure and unit-tested with faked execution
//! results. The sandboxed subprocess plumbing ([`evaluate_condition`],
//! [`evaluate_on_exit`]) wraps those pure functions.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tracing::{info, warn};

/// Maximum persisted condition state size in bytes (16 KiB). State larger than
/// this is rejected fail-closed (the evaluation does not fire and nothing is
/// written back).
pub const MAX_STATE_BYTES: usize = 16 * 1024;

/// Per-evaluation wall-clock timeout for a condition / watch command. A script
/// that overruns is killed and the evaluation fails closed (does not fire).
const EVAL_TIMEOUT_SECS: u64 = 30;

/// Env var carrying the prior persisted state into the condition script.
pub const ENV_CRON_STATE: &str = "DUDUCLAW_CRON_STATE";

// ── Trigger-kind state machine ─────────────────────────────────────────────

/// How a cron task decides whether to fire once its schedule slot is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Legacy behaviour — fire purely on the cron schedule.
    Time,
    /// Run [`CronTaskRow::condition_script`] and fire only when it reports
    /// `{fire:true}`.
    Condition,
    /// Run [`CronTaskRow::watch_command`] and fire only when it exits 0.
    OnExit,
}

impl TriggerKind {
    /// Lenient parse for values read out of the DB. Unknown / empty values fall
    /// back to [`TriggerKind::Time`] (the legacy, schedule-only behaviour) so a
    /// corrupt column never silently disables an existing task.
    pub fn from_db(s: &str) -> TriggerKind {
        match s.trim() {
            "condition" => TriggerKind::Condition,
            "on_exit" => TriggerKind::OnExit,
            _ => TriggerKind::Time,
        }
    }

    /// Strict parse for values arriving from an API/dashboard write. Returns
    /// `None` for anything unrecognised so a typo surfaces as an error instead
    /// of being silently coerced to `Time`.
    pub fn parse_strict(s: &str) -> Option<TriggerKind> {
        match s.trim() {
            "time" => Some(TriggerKind::Time),
            "condition" => Some(TriggerKind::Condition),
            "on_exit" => Some(TriggerKind::OnExit),
            _ => None,
        }
    }

    /// Canonical DB / wire string for this kind.
    pub fn as_db(&self) -> &'static str {
        match self {
            TriggerKind::Time => "time",
            TriggerKind::Condition => "condition",
            TriggerKind::OnExit => "on_exit",
        }
    }
}

// ── Condition output contract ──────────────────────────────────────────────

/// Raw `{fire, message?, state?}` shape emitted by a condition script.
#[derive(Debug, Deserialize)]
struct RawConditionOutput {
    fire: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    state: Option<serde_json::Value>,
}

/// A parsed, validated condition result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionOutput {
    pub fire: bool,
    pub message: Option<String>,
    /// Compact-serialized `state` object, guaranteed ≤ [`MAX_STATE_BYTES`].
    pub state: Option<String>,
}

/// Parse a condition script's stdout into a [`ConditionOutput`].
///
/// Robustness: scripts commonly print log lines before the JSON verdict, so if
/// the whole trimmed stdout does not parse we retry with the last non-empty
/// line. Any parse failure, a missing `fire` field, or a `state` object that
/// serializes to more than [`MAX_STATE_BYTES`] returns `Err` — the caller then
/// fails closed (does not fire).
pub fn parse_condition_output(stdout: &str) -> Result<ConditionOutput, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("empty stdout".to_string());
    }

    let raw: RawConditionOutput = match serde_json::from_str::<RawConditionOutput>(trimmed) {
        Ok(v) => v,
        Err(first_err) => {
            // Retry with the last non-empty line (logs-then-JSON pattern).
            let last_line = trimmed
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            serde_json::from_str::<RawConditionOutput>(last_line)
                .map_err(|_| format!("stdout is not valid condition JSON: {first_err}"))?
        }
    };

    let state = match raw.state {
        Some(v) if !v.is_null() => {
            let serialized =
                serde_json::to_string(&v).map_err(|e| format!("re-serialize state: {e}"))?;
            if serialized.len() > MAX_STATE_BYTES {
                return Err(format!(
                    "state {} bytes exceeds {MAX_STATE_BYTES}-byte cap",
                    serialized.len()
                ));
            }
            Some(serialized)
        }
        _ => None,
    };

    Ok(ConditionOutput {
        fire: raw.fire,
        message: raw.message.filter(|m| !m.is_empty()),
        state,
    })
}

/// The decision produced by evaluating a `condition` trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    /// Whether the task should fire this slot.
    pub fire: bool,
    /// Optional context message to inject into the fired prompt.
    pub message: Option<String>,
    /// Optional new state to persist for the next evaluation. `None` means
    /// "leave the stored state unchanged".
    pub new_state: Option<String>,
}

impl EvalOutcome {
    /// The fail-closed default: do not fire, change nothing.
    pub fn no_fire() -> Self {
        EvalOutcome {
            fire: false,
            message: None,
            new_state: None,
        }
    }
}

/// Map a condition script's raw execution result into a fire decision.
/// Fail-closed: an execution error or unparseable output never fires.
///
/// `exec` is `Ok(stdout)` whenever the process actually ran (regardless of its
/// exit code — the script's stdout JSON is authoritative), and `Err(reason)`
/// for spawn / sandbox / timeout failures.
pub fn interpret_condition_exec(exec: Result<String, String>) -> EvalOutcome {
    match exec {
        Ok(stdout) => match parse_condition_output(&stdout) {
            Ok(out) => EvalOutcome {
                fire: out.fire,
                message: out.message,
                new_state: out.state,
            },
            Err(e) => {
                warn!("條件腳本輸出解析失敗（fail-closed，不觸發）：{e}");
                EvalOutcome::no_fire()
            }
        },
        Err(e) => {
            warn!("條件腳本執行失敗（fail-closed，不觸發）：{e}");
            EvalOutcome::no_fire()
        }
    }
}

/// Decide whether an `on_exit` watch command's result should fire the task.
/// Fires only on a clean exit-0. Fail-closed for non-zero exit, signal
/// termination, or execution failure.
///
/// `exec` is `Ok(Some(code))` for a normal exit, `Ok(None)` for signal
/// termination, and `Err(reason)` for spawn / sandbox / timeout failures.
pub fn interpret_on_exit_exec(exec: Result<Option<i32>, String>) -> bool {
    match exec {
        Ok(Some(0)) => true,
        Ok(Some(code)) => {
            info!("on_exit 監看指令以狀態碼 {code} 結束（不觸發）");
            false
        }
        Ok(None) => {
            warn!("on_exit 監看指令被訊號中止（fail-closed，不觸發）");
            false
        }
        Err(e) => {
            warn!("on_exit 監看指令執行失敗（fail-closed，不觸發）：{e}");
            false
        }
    }
}

// ── Command planning (pure) ────────────────────────────────────────────────

/// Decide how to invoke a condition/watch payload. If the trimmed payload is an
/// existing regular file it is executed directly (respecting its shebang);
/// otherwise it is treated as an inline shell body run via `bash -c`.
///
/// Returns `(program, args)`.
pub fn plan_condition_command(payload: &str) -> (String, Vec<String>) {
    let trimmed = payload.trim();
    if !trimmed.is_empty() && Path::new(trimmed).is_file() {
        (trimmed.to_string(), Vec::new())
    } else {
        ("bash".to_string(), vec!["-c".to_string(), payload.to_string()])
    }
}

// ── Sandboxed execution (side-effecting) ───────────────────────────────────

/// Run `program args` confined by the native OS sandbox, scoped writable to
/// `cwd`, with the given env, capturing output under a wall-clock timeout.
///
/// Fail-closed: if the platform sandbox cannot confine the command (refused /
/// unsupported OS / error) the command is NOT run — `Err` is returned.
async fn run_sandboxed(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use duduclaw_core::types::SandboxLevel;
    use duduclaw_sandbox::{platform_sandbox, Confinement, SandboxSpec};

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        // Preserve a minimal PATH so `bash` and common tools resolve; the
        // sandbox confines writes, not the ambient PATH.
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/local/bin".to_string()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }

    // Confine writes to the ephemeral eval dir; reads default to `/` so the
    // interpreter and its libraries load. Network is left enabled (a condition
    // script may probe an API) — egress confinement is the container layer's
    // job, matching the agent-CLI sandbox policy.
    let spec = SandboxSpec::from_level(SandboxLevel::WorkspaceWrite, cwd);
    let sandbox = platform_sandbox();
    match sandbox.confine(cmd.as_std_mut(), &spec) {
        Ok(Confinement::Applied) => {}
        Ok(Confinement::Skipped) => {
            // WorkspaceWrite is never unconfined; treat as fail-closed.
            return Err(format!(
                "sandbox unexpectedly skipped confinement (availability: {:?})",
                sandbox.availability()
            ));
        }
        Ok(Confinement::Refused) => {
            return Err(format!(
                "native sandbox refused (availability: {:?}) — refusing to run script",
                sandbox.availability()
            ));
        }
        Err(e) => return Err(format!("sandbox confinement failed: {e}")),
    }

    let child = cmd.spawn().map_err(|e| format!("spawn {program}: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("wait: {e}")),
        Err(_elapsed) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

/// Evaluate a `condition` trigger: run `script` sandboxed, feed it `prior_state`
/// via `DUDUCLAW_CRON_STATE`, and interpret its stdout. Always fails closed.
pub async fn evaluate_condition(script: &str, prior_state: Option<&str>) -> EvalOutcome {
    if script.trim().is_empty() {
        warn!("condition trigger 缺少 condition_script（fail-closed，不觸發）");
        return EvalOutcome::no_fire();
    }

    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            warn!("建立條件評估暫存目錄失敗（fail-closed）：{e}");
            return EvalOutcome::no_fire();
        }
    };

    let (program, args) = plan_condition_command(script);
    let mut env = HashMap::new();
    env.insert(
        ENV_CRON_STATE.to_string(),
        prior_state.unwrap_or("").to_string(),
    );

    let exec = run_sandboxed(
        &program,
        &args,
        tmp.path(),
        &env,
        Duration::from_secs(EVAL_TIMEOUT_SECS),
    )
    .await
    .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());

    interpret_condition_exec(exec)
}

/// Evaluate an `on_exit` trigger: run `watch_command` sandboxed and fire only on
/// a clean exit-0. Always fails closed.
pub async fn evaluate_on_exit(watch_command: &str) -> bool {
    if watch_command.trim().is_empty() {
        warn!("on_exit trigger 缺少 watch_command（fail-closed，不觸發）");
        return false;
    }

    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            warn!("建立 on_exit 暫存目錄失敗（fail-closed）：{e}");
            return false;
        }
    };

    let args = vec!["-c".to_string(), watch_command.to_string()];
    let exec = run_sandboxed(
        "bash",
        &args,
        tmp.path(),
        &HashMap::new(),
        Duration::from_secs(EVAL_TIMEOUT_SECS),
    )
    .await
    .map(|out| out.status.code());

    interpret_on_exit_exec(exec)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TriggerKind state machine ──────────────────────────────────────

    #[test]
    fn trigger_kind_from_db_maps_known_and_defaults_unknown() {
        assert_eq!(TriggerKind::from_db("time"), TriggerKind::Time);
        assert_eq!(TriggerKind::from_db("condition"), TriggerKind::Condition);
        assert_eq!(TriggerKind::from_db("on_exit"), TriggerKind::OnExit);
        // Unknown / empty / typo → legacy-safe Time.
        assert_eq!(TriggerKind::from_db(""), TriggerKind::Time);
        assert_eq!(TriggerKind::from_db("conditon"), TriggerKind::Time);
        assert_eq!(TriggerKind::from_db("garbage"), TriggerKind::Time);
        // Surrounding whitespace tolerated.
        assert_eq!(TriggerKind::from_db("  condition "), TriggerKind::Condition);
    }

    #[test]
    fn trigger_kind_parse_strict_rejects_unknown() {
        assert_eq!(TriggerKind::parse_strict("time"), Some(TriggerKind::Time));
        assert_eq!(
            TriggerKind::parse_strict("condition"),
            Some(TriggerKind::Condition)
        );
        assert_eq!(
            TriggerKind::parse_strict("on_exit"),
            Some(TriggerKind::OnExit)
        );
        assert_eq!(TriggerKind::parse_strict("conditon"), None);
        assert_eq!(TriggerKind::parse_strict(""), None);
    }

    #[test]
    fn trigger_kind_as_db_roundtrips() {
        for kind in [TriggerKind::Time, TriggerKind::Condition, TriggerKind::OnExit] {
            assert_eq!(TriggerKind::from_db(kind.as_db()), kind);
            assert_eq!(TriggerKind::parse_strict(kind.as_db()), Some(kind));
        }
    }

    // ── parse_condition_output ─────────────────────────────────────────

    #[test]
    fn parse_minimal_fire_true() {
        let out = parse_condition_output(r#"{"fire": true}"#).unwrap();
        assert!(out.fire);
        assert_eq!(out.message, None);
        assert_eq!(out.state, None);
    }

    #[test]
    fn parse_fire_false_with_message_and_state() {
        let out =
            parse_condition_output(r#"{"fire": false, "message": "not yet", "state": {"n": 3}}"#)
                .unwrap();
        assert!(!out.fire);
        assert_eq!(out.message.as_deref(), Some("not yet"));
        // Compact re-serialization.
        assert_eq!(out.state.as_deref(), Some(r#"{"n":3}"#));
    }

    #[test]
    fn parse_empty_message_is_dropped() {
        let out = parse_condition_output(r#"{"fire": true, "message": ""}"#).unwrap();
        assert_eq!(out.message, None);
    }

    #[test]
    fn parse_logs_then_json_on_last_line() {
        let stdout = "checking upstream...\nfound 2 new items\n{\"fire\": true, \"message\": \"go\"}";
        let out = parse_condition_output(stdout).unwrap();
        assert!(out.fire);
        assert_eq!(out.message.as_deref(), Some("go"));
    }

    #[test]
    fn parse_missing_fire_is_error() {
        assert!(parse_condition_output(r#"{"message": "hi"}"#).is_err());
    }

    #[test]
    fn parse_empty_stdout_is_error() {
        assert!(parse_condition_output("   \n  ").is_err());
    }

    #[test]
    fn parse_malformed_json_is_error() {
        assert!(parse_condition_output("not json at all").is_err());
        assert!(parse_condition_output(r#"{"fire": tru"#).is_err());
    }

    #[test]
    fn parse_oversize_state_is_rejected() {
        // Build a state object whose serialization exceeds 16 KiB.
        let big = "x".repeat(MAX_STATE_BYTES + 100);
        let payload = format!(r#"{{"fire": true, "state": {{"blob": "{big}"}}}}"#);
        let err = parse_condition_output(&payload).unwrap_err();
        assert!(err.contains("exceeds"), "unexpected error: {err}");
    }

    #[test]
    fn parse_state_at_limit_is_accepted() {
        // A modestly sized state well under the cap round-trips.
        let payload = r#"{"fire": true, "state": {"cursor": "2026-07-11T00:00:00Z", "seen": [1,2,3]}}"#;
        let out = parse_condition_output(payload).unwrap();
        assert!(out.state.is_some());
        assert!(out.state.as_ref().unwrap().len() <= MAX_STATE_BYTES);
    }

    // ── interpret_condition_exec (fail-closed) ─────────────────────────

    #[test]
    fn interpret_exec_ok_fire_true() {
        let outcome = interpret_condition_exec(Ok(r#"{"fire": true, "state": {"a":1}}"#.into()));
        assert!(outcome.fire);
        assert_eq!(outcome.new_state.as_deref(), Some(r#"{"a":1}"#));
    }

    #[test]
    fn interpret_exec_error_fails_closed() {
        let outcome = interpret_condition_exec(Err("sandbox refused".into()));
        assert_eq!(outcome, EvalOutcome::no_fire());
    }

    #[test]
    fn interpret_exec_bad_output_fails_closed() {
        let outcome = interpret_condition_exec(Ok("garbage".into()));
        assert_eq!(outcome, EvalOutcome::no_fire());
    }

    #[test]
    fn interpret_exec_oversize_state_fails_closed() {
        let big = "x".repeat(MAX_STATE_BYTES + 100);
        let payload = format!(r#"{{"fire": true, "state": {{"blob": "{big}"}}}}"#);
        // Even though fire=true, the oversize state makes the whole verdict
        // unparseable → fail closed (do not fire).
        let outcome = interpret_condition_exec(Ok(payload));
        assert_eq!(outcome, EvalOutcome::no_fire());
    }

    // ── interpret_on_exit_exec ─────────────────────────────────────────

    #[test]
    fn on_exit_fires_only_on_zero() {
        assert!(interpret_on_exit_exec(Ok(Some(0))));
        assert!(!interpret_on_exit_exec(Ok(Some(1))));
        assert!(!interpret_on_exit_exec(Ok(Some(127))));
        assert!(!interpret_on_exit_exec(Ok(None))); // signal-killed
        assert!(!interpret_on_exit_exec(Err("spawn failed".into())));
    }

    // ── plan_condition_command ─────────────────────────────────────────

    #[test]
    fn plan_inline_uses_bash_dash_c() {
        let (prog, args) = plan_condition_command("echo '{\"fire\":true}'");
        assert_eq!(prog, "bash");
        assert_eq!(args, vec!["-c".to_string(), "echo '{\"fire\":true}'".to_string()]);
    }

    #[test]
    fn plan_existing_file_is_executed_directly() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.sh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/bash\necho '{{\"fire\":false}}'").unwrap();
        let path_str = path.to_string_lossy().to_string();
        let (prog, args) = plan_condition_command(&path_str);
        assert_eq!(prog, path_str);
        assert!(args.is_empty());
    }
}
