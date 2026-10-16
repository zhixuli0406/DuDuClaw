//! `duduclaw-maint-logs` — per-connection root service behind
//! `/run/duduclaw-maint-logs.sock` (`duduclaw-maint-logs.socket`,
//! `Accept=yes`, `StandardInput=socket`). Installed by the OS image as
//! `/usr/libexec/duduclaw/maint-logs`.
//!
//! fd 0 is the connected socket. Only uid 4800 (`duduclaw-maint`) is served;
//! the request is one of `gateway`, `gateway-follow`, `model`,
//! `model-follow`. See `duduclaw_maint_tools::logs_server`.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixStream;
    use std::process::ExitCode;

    use duduclaw_maint_tools::journal::{self, LOGS_IDENTIFIER, PRIORITY_INFO, PRIORITY_WARNING};
    use duduclaw_maint_tools::logs_server::{ServeOutcome, serve_connection, spawn_journalctl};
    use duduclaw_maint_tools::protocol::MAINT_UID;

    // Diagnostics go to journald, never to stderr: under systemd's defaults
    // stderr is the same socket as the client.
    let note = |priority: u8, message: &str| {
        let _ = journal::send(&journal::entry_payload(
            LOGS_IDENTIFIER,
            priority,
            message,
            &[],
        ));
    };

    let fd = match std::io::stdin().as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(e) => {
            note(
                PRIORITY_WARNING,
                &format!("maint-logs: cannot use stdin: {e}"),
            );
            return ExitCode::FAILURE;
        }
    };
    let file = std::fs::File::from(fd);
    match file.metadata() {
        Ok(m) if m.file_type().is_socket() => {}
        _ => {
            note(
                PRIORITY_WARNING,
                "maint-logs: stdin is not a socket; start it through duduclaw-maint-logs.socket",
            );
            return ExitCode::FAILURE;
        }
    }
    let stream = UnixStream::from(std::os::fd::OwnedFd::from(file));

    match serve_connection(&stream, MAINT_UID, spawn_journalctl, &chrono::Local) {
        ServeOutcome::Refused(why) => {
            note(
                PRIORITY_WARNING,
                &format!("maint-logs: refused connection: {why}"),
            );
            // A refused client is not a service fault: exiting non-zero
            // would leave one failed instance unit per refusal, cluttering
            // `systemctl --failed` (which the menu's `status` shows).
            ExitCode::SUCCESS
        }
        ServeOutcome::BadRequest(_) => {
            note(
                PRIORITY_WARNING,
                "maint-logs: refused request (not one of the four tokens)",
            );
            ExitCode::SUCCESS
        }
        ServeOutcome::Unavailable(e) => {
            note(
                PRIORITY_WARNING,
                &format!("maint-logs: journalctl could not start: {e}"),
            );
            ExitCode::FAILURE
        }
        ServeOutcome::Streamed {
            request,
            stats,
            client_gone,
            exit_code,
        } => {
            note(
                PRIORITY_INFO,
                &format!(
                    "maint-logs: served {} ({} lines, {} withheld, {} malformed, {} oversized{}{})",
                    request.token(),
                    stats.emitted,
                    stats.withheld,
                    stats.malformed,
                    stats.too_long,
                    if stats.capped { ", capped" } else { "" },
                    if client_gone { ", client left" } else { "" },
                ),
            );
            if client_gone || stats.capped || exit_code == Some(0) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("duduclaw-maint-logs runs only on DuDuClaw OS (Linux)");
    std::process::ExitCode::FAILURE
}
