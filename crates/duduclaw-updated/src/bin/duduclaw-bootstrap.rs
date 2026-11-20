//! duduclaw-bootstrap: the independently hash-checked root transition verifier
//! (plan R1). It takes no path argument: the bundle is read only from the fixed
//! root-only inbox, and on success this process becomes the verified gateway's
//! `service bootstrap-continue`. stderr carries only a step number, an error code
//! and fixed zh-TW guidance; never a path, file content, OS error text, panic
//! message or backtrace.
use duduclaw_updated::bootstrap_cli::{self, Command};

const USAGE: &str = "用法：duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>]... [--enable-now] | upgrade | rollback | --print-root-pin | --version";

/// Installed before anything else runs: replaces the default hook, so a panic
/// prints one fixed line (no message, location or backtrace; RUST_BACKTRACE has
/// no effect once the default hook is gone).
fn quiet_panics() {
    std::panic::set_hook(Box::new(|_| {
        use std::io::Write;
        let _ = std::io::stderr().write_all(b"bootstrap_refused: internal_error\n");
    }));
}

/// Step number, error code and the step's fixed hint; nothing else.
#[cfg(target_os = "linux")]
fn refuse(refusal: duduclaw_updated::linux::bootstrap_verify::Refusal) -> ! {
    eprintln!("bootstrap_refused: step {} {:?}", refusal.step.number(), refusal.error.code());
    eprintln!("{}", refusal.hint());
    std::process::exit(1);
}

/// Parse argv and refuse a state-changing command inside an AI session: one
/// fixed line (never the marker or its value), exit 1. Runs before anything
/// else (hardening, verification, high-water write, staging, locks), because
/// the verified gateway is exec'd with a cleared environment and so its own
/// operator gate never sees the markers. A guard against mistakes, not a
/// privilege boundary: a process can unset its environment.
fn parse_or_refuse_ai_session() -> (Command, bootstrap_cli::Forwarded) {
    match bootstrap_cli::parse_guarded(std::env::args_os().skip(1), bootstrap_cli::process_env_presence) {
        Ok(parsed) => parsed,
        Err(refused) => {
            eprintln!("{}", refused.line());
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    use duduclaw_updated::linux::{self as platform, bootstrap_verify::{self, BootstrapMode, Refusal, Step}};
    quiet_panics();
    // Only `install` carries options: three typed values, re-rendered from their
    // parsed types and forwarded to `service bootstrap-continue` (never a path).
    let (command, forwarded) = parse_or_refuse_ai_session();
    if command.changes_state() {
        // A state-changing command was selected: harden before any other work.
        if let Err(error) = platform::harden() { refuse(Refusal::at(Step::Harden, error)); }
    }
    let mode = match command {
        Command::PrintRootPin => match bootstrap_verify::pinned_root() {
            Ok((digest, kind)) => {
                println!("{} {}", kind.label(), hex::encode(digest.0));
                std::process::exit(0);
            }
            Err(_) => {
                println!("UNSET");
                std::process::exit(1);
            }
        },
        Command::Version => {
            println!("{}", bootstrap_cli::version_line());
            std::process::exit(0);
        }
        Command::Install => BootstrapMode::Install,
        Command::Upgrade => BootstrapMode::Upgrade,
        Command::Rollback => BootstrapMode::Rollback,
        Command::Usage => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match bootstrap_verify::run_with(mode, &forwarded.args()) {
        Ok(never) => match never {},
        Err(refusal) => refuse(refusal),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    quiet_panics();
    if parse_or_refuse_ai_session().0 == Command::Version {
        println!("{}", bootstrap_cli::version_line());
        return;
    }
    eprintln!("duduclaw-bootstrap requires native Linux");
    std::process::exit(1);
}
