//! `duduclaw-maint-tools` — the two small programs behind the DuDuClaw OS
//! maintenance SSH account (`duduclaw-maint`, uid/gid 4800).
//!
//! - `duduclaw-maint-shell` (installed as `/usr/libexec/duduclaw/maint-shell`)
//!   is both the account's login shell and dropbear's forced command. It
//!   offers a closed menu: status, gateway logs, local model logs,
//!   resources, network, restart the local model, restart the gateway. No
//!   reboot, no general shell, no argument from the client ever reaches a
//!   child process.
//! - `duduclaw-maint-logs` (installed as `/usr/libexec/duduclaw/maint-logs`)
//!   is a root service behind a `0660 root:duduclaw-maint` unix socket
//!   (systemd `Accept=yes`). It checks the peer's uid with `SO_PEERCRED`,
//!   accepts one of four fixed tokens, reads the gateway or local-model
//!   journal as JSON, drops the first-run bootstrap password line, redacts
//!   credentials and returns bounded, plain-text lines.
//!
//! Design reference: maintenance SSH account design §5.1, §5.2 and §5.3
//! (internal document; this crate's README summarises what the code relies
//! on). The account itself, the socket/service units, the polkit rule and
//! the dropbear configuration ship in the DuDuClaw-OS repo.
//!
//! Module map:
//! - [`protocol`] — constants shared by both programs (uid 4800, socket
//!   path, unit names) and the closed log-request token set.
//! - [`menu`] — the menu's actions, the exact `SSH_ORIGINAL_COMMAND`
//!   grammar, the fixed argv each action runs, and the executor-generic
//!   dispatch used by both the interactive and the command mode.
//! - [`logs_filter`] — journald JSON record → one redacted text line;
//!   bounded line reading; the record streaming loop.
//! - [`peer`] — peer-credential lookup and the uid gate.
//! - [`sanitize`] — control/bidi character removal for terminal output.
//! - [`tools`] — the fixed program list, fixed search directories and the
//!   cleared child environment.
//! - [`journal`] — the native-protocol journald entry the menu writes for
//!   every action (`SYSLOG_IDENTIFIER=duduclaw-maint`).
//! - [`logs_client`] — the shell's side of the log socket (unix only).
//! - [`logs_server`] — one connection of the log service: peer gate, token,
//!   `journalctl`, hang-up watcher (unix only).
//! - [`signals`] — SIGINT flag so Ctrl-C returns to the menu (unix only).
//! - [`system`] — the real executor that spawns `systemctl` & co. (unix only).

pub mod journal;
pub mod logs_filter;
pub mod menu;
pub mod peer;
pub mod protocol;
pub mod sanitize;
pub mod tools;

#[cfg(unix)]
pub mod logs_client;
#[cfg(unix)]
pub mod logs_server;
#[cfg(unix)]
pub mod signals;
#[cfg(unix)]
pub mod system;
