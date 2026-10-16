//! `duduclaw-maint-shell` — login shell and forced command of the
//! `duduclaw-maint` account. Installed by the OS image as
//! `/usr/libexec/duduclaw/maint-shell` and listed in `/etc/shells`.
//!
//! dropbear runs it as `maint-shell -c <forced command>` for every session
//! (`-c /usr/libexec/duduclaw/maint-shell`), putting whatever the client
//! asked for in `SSH_ORIGINAL_COMMAND`. Its own arguments are ignored. No
//! `SSH_ORIGINAL_COMMAND` → interactive menu; otherwise the command must be
//! one of the documented ones word for word (exit code 2 if not). See
//! `duduclaw_maint_tools::menu`.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use duduclaw_maint_tools::{menu, signals, system::SystemExecutor};

    let mut exec = SystemExecutor;
    if let Err(e) = signals::install_sigint_handler() {
        // Without it Ctrl-C ends the session instead of returning to the
        // menu; still usable, so say so and continue.
        menu::Executor::warn(
            &mut exec,
            &format!("（Ctrl-C 會直接結束連線 / Ctrl-C will end the session: {e}）\n"),
        );
    }

    // `var_os` + lossy: a non-UTF-8 command cannot match anything anyway and
    // is refused like any other unknown command.
    let code = match std::env::var_os("SSH_ORIGINAL_COMMAND") {
        Some(cmd) => menu::run_command_mode(&cmd.to_string_lossy(), &mut exec),
        None => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            menu::run_interactive(&mut input, &mut exec)
        }
    };
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("duduclaw-maint-shell runs only on DuDuClaw OS (Linux)");
    std::process::ExitCode::FAILURE
}
