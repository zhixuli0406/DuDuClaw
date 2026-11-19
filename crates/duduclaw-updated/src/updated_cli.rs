//! `duduclaw-updated` (installed as `/usr/libexec/duduclaw-updated-launch`) argv.
//! Pure and platform-neutral so it is tested everywhere. Arguments are compared
//! as `OsStr` against fixed literals: non-UTF-8 input is a usage error, never a
//! panic, and nothing is ever converted lossily. At most three items are pulled.
//!
//! Every mode is classified ([`UpdatedCommand::class`], an exhaustive match) as
//! the systemd service, a read-only query or a state-changing operator command;
//! the last kind is refused inside an AI session (the marker list
//! `duduclaw_isolation::ai_session::AI_SESSION_ENV_VARS`, shared with
//! duduclaw-cli and `duduclaw-bootstrap`) with the bootstrap binary's fixed line,
//! before any file, socket or lock is touched.
use std::ffi::OsString;
use duduclaw_isolation::update::UpdateSlot;
use crate::bootstrap_cli::AiSessionRefused;

/// The whole command line after argv[0]; each mode is one fixed argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatedCommand {
    /// `--launch`: the unit's `ExecStart` (duduclaw-updated.service). Frozen
    /// launcher: one durable boot attempt, then execs `--serve-slot`.
    Launch,
    /// `--serve-slot a|b`: exec'd only by [`UpdatedCommand::Launch`] with a fixed
    /// two-variable environment; the long-running update service.
    ServeSlot(UpdateSlot),
    /// `--check-latest --dry-run`: discovers a candidate, prints it, sends nothing.
    CheckLatestDryRun,
    /// `--check-latest`: discovers a candidate and sends `Apply` (queues a host upgrade).
    CheckLatest,
    /// `--bootstrap-stage`: publishes an inactive generation from the fixed inbox.
    BootstrapStage,
    /// `--rotate-root`: rotates the trust root from `root-update.json`.
    RotateRoot,
    /// `--emergency`: prepares a signed emergency rollback.
    Emergency,
    /// Anything else, including extra, empty or non-UTF-8 arguments. Refused as
    /// an invalid request without any I/O.
    Usage,
}

/// How a mode relates to the AI-session guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Started by systemd (or by the launcher's exec), whose environment never
    /// carries a marker. Never refused; argv is exact, so no operator action
    /// flag can be appended to it.
    Service,
    /// Writes no host state; allowed inside an AI session.
    ReadOnly,
    /// Changes root state; refused inside an AI session.
    ChangesState,
}

impl UpdatedCommand {
    /// Every mode, for tests (and kept in step with [`Self::class`] by its test).
    pub const ALL: [Self; 9] = [Self::Launch, Self::ServeSlot(UpdateSlot::A), Self::ServeSlot(UpdateSlot::B),
        Self::CheckLatestDryRun, Self::CheckLatest, Self::BootstrapStage, Self::RotateRoot, Self::Emergency,
        Self::Usage];

    /// Exhaustive on purpose (no wildcard arm): a new mode does not compile
    /// until it is classified here.
    pub fn class(self) -> Class {
        match self {
            // systemd's ExecStart and the launcher's exec (fixed PATH/LANG env).
            Self::Launch | Self::ServeSlot(UpdateSlot::A) | Self::ServeSlot(UpdateSlot::B) => Class::Service,
            // Dry run: reads state and fetches the release index as duduclaw-dl;
            // writes nothing and never connects to the update socket.
            // Usage: returns `Invalid` before any I/O.
            Self::CheckLatestDryRun | Self::Usage => Class::ReadOnly,
            // Apply request over the control socket / generation publication /
            // trust ledger write / emergency pending write.
            Self::CheckLatest | Self::BootstrapStage | Self::RotateRoot | Self::Emergency => Class::ChangesState,
        }
    }
}

/// Parse `std::env::args_os().skip(1)`. Only the exact argv of a mode is
/// accepted; anything else (extra items included) is [`UpdatedCommand::Usage`].
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> UpdatedCommand {
    let mut args = args.into_iter();
    let first = args.next();
    let second = args.next();
    if args.next().is_some() { return UpdatedCommand::Usage; }
    let Some(first) = first else { return UpdatedCommand::Usage };
    match second {
        None => {
            if first == *"--launch" {
                UpdatedCommand::Launch
            } else if first == *"--bootstrap-stage" {
                UpdatedCommand::BootstrapStage
            } else if first == *"--check-latest" {
                UpdatedCommand::CheckLatest
            } else if first == *"--rotate-root" {
                UpdatedCommand::RotateRoot
            } else if first == *"--emergency" {
                UpdatedCommand::Emergency
            } else {
                UpdatedCommand::Usage
            }
        }
        Some(second) => {
            if first == *"--serve-slot" && second == *"a" {
                UpdatedCommand::ServeSlot(UpdateSlot::A)
            } else if first == *"--serve-slot" && second == *"b" {
                UpdatedCommand::ServeSlot(UpdateSlot::B)
            } else if first == *"--check-latest" && second == *"--dry-run" {
                UpdatedCommand::CheckLatestDryRun
            } else {
                UpdatedCommand::Usage
            }
        }
    }
}

/// Whether `command` must be refused because an AI-session marker is present
/// according to `env` (presence only). Only [`Class::ChangesState`] is refused.
/// Pure: no I/O.
///
/// A guard against mistakes, not a privilege boundary: a process can unset its
/// environment.
pub fn refuse_ai_session(command: UpdatedCommand, env: impl Fn(&str) -> Option<String>) -> bool {
    let guarded = match command.class() {
        Class::Service | Class::ReadOnly => false,
        Class::ChangesState => true,
    };
    guarded && duduclaw_isolation::ai_session::in_ai_session(&|key| env(key).is_some())
}

/// [`parse`] followed at once by [`refuse_ai_session`]: the first thing
/// `duduclaw-updated` does, before hardening, any file open, socket connect or
/// lock. On `Err` the binary prints [`AiSessionRefused::line`] (the same fixed
/// line as `duduclaw-bootstrap`) and exits 1. Pure: reads only `args` and `env`.
pub fn parse_guarded<I: IntoIterator<Item = OsString>>(
    args: I,
    env: impl Fn(&str) -> Option<String>,
) -> Result<UpdatedCommand, AiSessionRefused> {
    let command = parse(args);
    if refuse_ai_session(command, env) { return Err(AiSessionRefused); }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap_cli::AI_SESSION_REFUSAL;

    fn args(list: &[&str]) -> Vec<OsString> { list.iter().map(OsString::from).collect() }

    /// The argv that selects each mode (Usage: one representative).
    fn argv(command: UpdatedCommand) -> &'static [&'static str] {
        match command {
            UpdatedCommand::Launch => &["--launch"],
            UpdatedCommand::ServeSlot(UpdateSlot::A) => &["--serve-slot", "a"],
            UpdatedCommand::ServeSlot(UpdateSlot::B) => &["--serve-slot", "b"],
            UpdatedCommand::CheckLatestDryRun => &["--check-latest", "--dry-run"],
            UpdatedCommand::CheckLatest => &["--check-latest"],
            UpdatedCommand::BootstrapStage => &["--bootstrap-stage"],
            UpdatedCommand::RotateRoot => &["--rotate-root"],
            UpdatedCommand::Emergency => &["--emergency"],
            UpdatedCommand::Usage => &["help"],
        }
    }

    #[test]
    fn every_updated_mode_is_listed_and_classified() {
        // `argv` and `class` are exhaustive matches; ALL must name each variant once.
        let mut seen = std::collections::BTreeSet::new();
        for command in UpdatedCommand::ALL {
            assert!(seen.insert(format!("{command:?}")), "{command:?} listed twice");
            assert_eq!(parse(args(argv(command))), command, "{command:?}");
        }
        assert_eq!(seen.len(), UpdatedCommand::ALL.len());
        let of = |class: Class| UpdatedCommand::ALL.into_iter().filter(|c| c.class() == class).collect::<Vec<_>>();
        assert_eq!(of(Class::Service), [UpdatedCommand::Launch, UpdatedCommand::ServeSlot(UpdateSlot::A),
            UpdatedCommand::ServeSlot(UpdateSlot::B)]);
        assert_eq!(of(Class::ReadOnly), [UpdatedCommand::CheckLatestDryRun, UpdatedCommand::Usage]);
        assert_eq!(of(Class::ChangesState), [UpdatedCommand::CheckLatest, UpdatedCommand::BootstrapStage,
            UpdatedCommand::RotateRoot, UpdatedCommand::Emergency]);
    }

    #[test]
    fn updated_argv_is_exact_and_service_mode_takes_no_operator_flag() {
        let usage: Vec<Vec<&str>> = vec![vec![], vec![""], vec!["--LAUNCH"], vec!["--launch "], vec!["launch"],
            vec!["--launch", "--rotate-root"], vec!["--launch", "--emergency"], vec!["--launch", "--check-latest"],
            vec!["--launch", "--bootstrap-stage"], vec!["--rotate-root", "--launch"],
            vec!["--serve-slot"], vec!["--serve-slot", "c"], vec!["--serve-slot", "A"], vec!["--serve-slot", "a", "b"],
            vec!["--serve-slot", "a", "--rotate-root"], vec!["--serve-slot", "a", "--emergency"],
            vec!["--serve-slot", "a", "--check-latest"], vec!["--serve-slot", "--rotate-root"],
            vec!["--check-latest", "--dry-run", "--dry-run"], vec!["--dry-run", "--check-latest"],
            vec!["--check-latest", "--rotate-root"], vec!["--rotate-root", "--dry-run"], vec!["--emergency", "--dry-run"],
            vec!["--bootstrap-stage", "--dry-run"], vec!["--check-latest=--dry-run"], vec!["--version"]];
        for bad in &usage {
            assert_eq!(parse(args(bad)), UpdatedCommand::Usage, "{bad:?}");
        }
        // An unbounded iterator terminates.
        assert_eq!(parse(std::iter::repeat(OsString::from("--launch"))), UpdatedCommand::Usage);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_updated_argv_is_a_usage_error_not_a_panic() {
        use std::os::unix::ffi::OsStringExt;
        for bytes in [&b"\xff"[..], b"--launch\xff", b"--rotate-root\x80"] {
            let raw = || OsString::from_vec(bytes.to_vec());
            assert_eq!(parse(vec![raw()]), UpdatedCommand::Usage);
            assert_eq!(parse(vec![OsString::from("--serve-slot"), raw()]), UpdatedCommand::Usage);
            assert_eq!(parse(vec![OsString::from("--check-latest"), raw()]), UpdatedCommand::Usage);
        }
    }

    #[test]
    fn every_state_changing_updated_mode_is_refused_with_each_ai_session_marker() {
        let markers = duduclaw_isolation::ai_session::AI_SESSION_ENV_VARS;
        assert!(!markers.is_empty());
        for marker in markers {
            for value in ["", "x"] {
                let env = |key: &str| (key == *marker).then(|| value.to_owned());
                for command in UpdatedCommand::ALL {
                    assert_eq!(refuse_ai_session(command, env), command.class() == Class::ChangesState,
                        "{command:?} {marker}");
                }
                for command in [UpdatedCommand::CheckLatest, UpdatedCommand::BootstrapStage,
                    UpdatedCommand::RotateRoot, UpdatedCommand::Emergency] {
                    assert_eq!(parse_guarded(args(argv(command)), env), Err(AiSessionRefused), "{command:?} {marker}");
                }
                // The service and read-only modes pass inside an AI session.
                for command in [UpdatedCommand::Launch, UpdatedCommand::ServeSlot(UpdateSlot::A),
                    UpdatedCommand::ServeSlot(UpdateSlot::B), UpdatedCommand::CheckLatestDryRun, UpdatedCommand::Usage] {
                    assert_eq!(parse_guarded(args(argv(command)), env), Ok(command), "{command:?} {marker}");
                }
            }
        }
        // No marker or only unrelated variables: nothing is refused.
        for command in UpdatedCommand::ALL {
            assert!(!refuse_ai_session(command, |_| None));
            assert!(!refuse_ai_session(command, |key| (key == "PATH" || key == "SUDO_USER" || key == "LANG")
                .then(String::new)));
            assert_eq!(parse_guarded(args(argv(command)), |_| None), Ok(command));
        }
        // The same fixed line as duduclaw-bootstrap, naming no marker.
        assert_eq!(AiSessionRefused.line(), AI_SESSION_REFUSAL);
        for marker in markers { assert!(!AI_SESSION_REFUSAL.contains(marker)); }
    }

    #[test]
    fn updated_entry_refuses_in_an_ai_session_before_anything_else() {
        // `parse_guarded` is the binary's first step; it reads only argv and the env
        // callback, so a refusal here happens before hardening, any file open,
        // socket connect or lock (the binary exits on Err without calling anything else).
        let marked = |key: &str| (key == "DUDUCLAW_AGENT_ID").then(String::new);
        let looked_up = std::cell::RefCell::new(Vec::new());
        let recording = |key: &str| {
            looked_up.borrow_mut().push(key.to_owned());
            marked(key)
        };
        for word in [&["--check-latest"][..], &["--bootstrap-stage"], &["--rotate-root"], &["--emergency"]] {
            assert_eq!(parse_guarded(args(word), recording), Err(AiSessionRefused), "{word:?}");
        }
        // Only marker names were consulted.
        let markers = duduclaw_isolation::ai_session::AI_SESSION_ENV_VARS;
        assert!(!looked_up.borrow().is_empty());
        assert!(looked_up.borrow().iter().all(|key| markers.contains(&key.as_str())));
        // Service and read-only modes do not even consult the environment.
        looked_up.borrow_mut().clear();
        for word in [&["--launch"][..], &["--serve-slot", "a"], &["--serve-slot", "b"], &["--check-latest", "--dry-run"],
            &["help"]] {
            assert!(parse_guarded(args(word), recording).is_ok(), "{word:?}");
        }
        assert!(looked_up.borrow().is_empty());
        // Outside an AI session the parse result is unchanged.
        assert_eq!(parse_guarded(args(&["--check-latest"]), |_| None), Ok(UpdatedCommand::CheckLatest));
        assert_eq!(parse_guarded(args(&["--emergency"]), |_| None), Ok(UpdatedCommand::Emergency));
    }
}
