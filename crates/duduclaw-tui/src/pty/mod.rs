//! Owner-local terminal. Child bytes have exactly one destination: the VT model.
mod input;
mod writer;
#[cfg(unix)]
mod process;
#[cfg(test)]
mod tests;
use crate::strings;
pub use input::{escape, key_bytes, paste_bytes};
use portable_pty::{CommandBuilder, PtySize};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex, mpsc},
};

// The gateway's runtime/grok.rs also leaves PTY_TREE_CLEANUP_VERIFIED false.
// Its Job Object launcher cannot yet wrap portable-pty's ConPTY spawn atomically.
pub const SUPPORTED: bool = cfg!(unix);
pub const SCROLLBACK: usize = 5_000;
pub const MAX_INPUT: usize = writer::MAX_PENDING;
pub type Shared = Arc<Mutex<Session>>;

pub struct Session {
    pub parser: vt100::Parser,
    pub status: String,
    #[cfg(unix)]
    process: Option<process::Process>,
    output: Option<mpsc::Receiver<Vec<u8>>>,
    input: Option<writer::InputWriter>,
    input_batch: Vec<u8>,
    batching_input: bool,
    started: bool,
    pub cleanup_complete: bool,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            parser: vt100::Parser::new(24, 80, SCROLLBACK),
            status: strings::PTY_STARTING.into(),
            #[cfg(unix)]
            process: None,
            output: None,
            input: None,
            input_batch: Vec::new(),
            batching_input: false,
            started: false,
            cleanup_complete: true,
        }
    }
}
impl Session {
    pub fn shared() -> Shared {
        Arc::new(Mutex::new(Self::default()))
    }
    pub fn start(&mut self, command: Option<&[String]>) {
        if self.started {
            return;
        }
        self.started = true;
        #[cfg(not(unix))]
        {
            let _ = command;
            self.status = strings::PTY_DISABLED.into();
        }
        #[cfg(unix)]
        match process::Process::spawn(command, self.size()) {
            Ok((process, output, input)) => {
                self.process = Some(process);
                self.output = Some(output);
                self.input = Some(input);
                self.status = strings::PTY_RUNNING.into();
            }
            Err(_) => self.status = strings::PTY_START_FAILED.into(),
        }
    }
    pub fn running(&self) -> bool {
        #[cfg(unix)]
        {
            self.process.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    pub fn pending_or_running(&self) -> bool {
        SUPPORTED && (!self.started || self.running())
    }
    pub fn poll(&mut self) {
        if self.input.as_ref().is_some_and(writer::InputWriter::failed) {
            self.status = strings::PTY_INPUT_FAILED.into();
        }
        // Bound work per frame; the reader's bounded channel applies backpressure.
        if let Some(output) = &self.output {
            for _ in 0..32 {
                let Ok(bytes) = output.try_recv() else {
                    break;
                };
                self.parser.process(&bytes);
            }
        }
        #[cfg(unix)]
        if self.process.as_ref().is_some_and(process::Process::exited) {
            if let Some(mut process) = self.process.take() {
                let status = process.finish();
                self.status = format!(
                    "{} {}",
                    strings::PTY_EXITED,
                    status
                        .map(|s| s.exit_code().to_string())
                        .unwrap_or_else(|_| "?".into())
                );
                self.cleanup_complete = process.cleanup_complete;
                if !self.cleanup_complete {
                    self.status
                        .push_str(&format!("; {}", strings::PTY_STOP_FAILED));
                }
            }
            self.input = None;
        }
    }
    pub fn size(&self) -> PtySize {
        let (rows, cols) = self.parser.screen().size();
        PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.clamp(1, 200);
        let cols = cols.clamp(2, 500);
        if self.parser.screen().size() == (rows, cols) {
            return;
        }
        #[cfg(unix)]
        if let Some(process) = &self.process
            && process.resize(rows, cols).is_err()
        {
            self.status = strings::PTY_RESIZE_FAILED.into();
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
    }
    pub fn begin_input_batch(&mut self) { self.batching_input = true; }
    pub fn flush_input(&mut self) {
        self.batching_input = false;
        let bytes = std::mem::take(&mut self.input_batch);
        self.send(bytes);
    }
    pub fn input_backpressured(&self) -> bool {
        self.input.as_ref().is_some_and(|input| !input.failed()
            && input.pending() + self.input_batch.len() >= writer::BACKPRESSURE_AT)
    }
    pub fn send(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() { return; }
        let Some(input) = &self.input else {
            self.status = strings::PTY_INPUT_FAILED.into();
            return;
        };
        if bytes.len() > MAX_INPUT.saturating_sub(input.pending() + self.input_batch.len()) {
            self.status = strings::PTY_INPUT_FULL.into();
        } else if self.batching_input && !input.failed() {
            self.input_batch.extend(bytes);
        } else if input.send(bytes).is_err() {
            self.status = strings::PTY_INPUT_FAILED.into();
        }
    }
    pub fn scroll(&mut self, up: bool) {
        let screen = self.parser.screen_mut();
        let page = usize::from(screen.size().0.saturating_sub(1).max(1));
        let offset = screen.scrollback();
        screen.set_scrollback(if up {
            offset.saturating_add(page)
        } else {
            offset.saturating_sub(page)
        });
    }
    pub fn stop(&mut self) {
        self.input_batch.clear();
        self.batching_input = false;
        self.input = None;
        // Drop the receiver before joining/reaping, unblocking a full reader queue.
        self.output = None;
        #[cfg(unix)]
        if let Some(mut process) = self.process.take() {
            let exited = process.finish().is_ok();
            self.cleanup_complete = exited && process.cleanup_complete;
            self.status = if self.cleanup_complete {
                strings::PTY_STOPPED
            } else {
                strings::PTY_STOP_FAILED
            }
            .into();
        }
        self.started = true;
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// No auth/session argument exists: JWTs stay in the socket actors' memory.
/// Inherit owner variables, removing every DuDuClaw marker, including unknown ones.
pub(crate) fn command_with_env(
    command: Option<&[String]>,
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> CommandBuilder {
    let env: Vec<_> = env.into_iter().collect();
    let shell_key = if cfg!(windows) { "ComSpec" } else { "SHELL" };
    let shell = env
        .iter()
        .find(|(k, v)| k == shell_key && !v.is_empty())
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into());
    let mut cmd = if let Some(args) = command.filter(|a| !a.is_empty()) {
        let mut cmd = CommandBuilder::new(&args[0]);
        cmd.args(&args[1..]);
        cmd
    } else {
        CommandBuilder::new(shell)
    };
    cmd.env_clear();
    for (key, value) in env {
        if !key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("DUDUCLAW_")
        {
            cmd.env(key, value);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd
}
