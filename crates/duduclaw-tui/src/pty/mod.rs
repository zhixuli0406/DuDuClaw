//! Owner-local terminal. Child bytes have exactly one destination: the VT model.
mod input;
mod filter;
mod parser;
pub use parser::Screen;
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
    sync::{Arc, Mutex},
};

// The gateway's runtime/grok.rs also leaves PTY_TREE_CLEANUP_VERIFIED false.
// Its Job Object launcher cannot yet wrap portable-pty's ConPTY spawn atomically.
pub const SUPPORTED: bool = cfg!(unix);
pub const SCROLLBACK: usize = 5_000;
pub const MAX_INPUT: usize = writer::MAX_PENDING;
pub type Shared = Arc<Mutex<Session>>;

pub struct Session {
    screen: Arc<Screen>,
    parser: Option<parser::ParserWorker>,
    dimensions: (u16, u16),
    scroll_offset: usize,
    pub status: String,
    #[cfg(unix)]
    process: Option<process::Process>,
    input: Option<writer::InputWriter>,
    input_batch: Vec<u8>,
    batching_input: bool,
    started: bool,
    /// True only before a supported launch attempt; detached descendants cannot be ruled out.
    pub cleanup_complete: bool,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            screen: Arc::new(Screen::capture(vt100::Parser::new(24, 80, 0).screen())),
            parser: None,
            dimensions: (24, 80),
            scroll_offset: 0,
            status: strings::PTY_STARTING.into(),
            #[cfg(unix)]
            process: None,
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
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        // A failed spawn may already have created a child before worker setup.
        self.cleanup_complete = !SUPPORTED;
        #[cfg(not(unix))]
        {
            self.status = strings::PTY_DISABLED.into();
        }
        #[cfg(unix)]
        match process::Process::spawn(self.size()) {
            Ok((process, output, input)) => {
                let (rows, cols) = self.dimensions;
                match parser::ParserWorker::spawn(output, rows, cols) {
                    Ok(parser) => self.parser = Some(parser),
                    Err(_) => { self.status = strings::PTY_START_FAILED.into(); return; }
                }
                self.process = Some(process);
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
        if let Some(screen) = self.parser.as_ref().and_then(parser::ParserWorker::screen) {
            self.screen = screen;
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
        let (rows, cols) = self.dimensions;
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
        if self.dimensions == (rows, cols) {
            return;
        }
        #[cfg(unix)]
        if let Some(process) = &self.process
            && process.resize(rows, cols).is_err()
        {
            self.status = strings::PTY_RESIZE_FAILED.into();
            return;
        }
        self.dimensions = (rows, cols);
        if let Some(parser) = &self.parser { parser.resize(rows, cols); }
    }
    pub fn begin_input_batch(&mut self) { self.batching_input = true; }
    pub fn flush_input(&mut self) {
        self.batching_input = false;
        let bytes = std::mem::take(&mut self.input_batch);
        self.send(bytes);
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
    pub fn screen(&self) -> &Screen { &self.screen }
    pub fn reset_scroll(&mut self) {
        self.scroll_offset = 0;
        if let Some(parser) = &self.parser { parser.scroll(0); }
    }
    pub fn scroll(&mut self, up: bool) {
        let page = usize::from(self.dimensions.0.saturating_sub(1).max(1));
        self.scroll_offset = if up {
            self.scroll_offset.saturating_add(page).min(SCROLLBACK)
        } else {
            self.scroll_offset.saturating_sub(page)
        };
        if let Some(parser) = &self.parser { parser.scroll(self.scroll_offset); }
    }
    #[cfg(test)]
    pub(crate) fn set_screen_for_test(&mut self, screen: &vt100::Screen) {
        self.screen = Arc::new(Screen::capture(screen));
    }
    #[cfg(all(test, unix))]
    fn start_script_for_test(&mut self, script: &str) {
        self.start();
        self.send(format!("exec /bin/sh -c '{}'\n", script.replace('\'', "'\\''")).into_bytes());
    }
    #[cfg(test)]
    pub(crate) fn set_writer_for_test(&mut self, writer: impl std::io::Write + Send + 'static) {
        self.input = Some(writer::InputWriter::spawn(writer).unwrap());
    }
    pub fn stop(&mut self) {
        self.input_batch.clear();
        self.batching_input = false;
        self.input = None;
        // Cancellation never joins a parser or waits for its current chunk.
        self.parser = None;
        #[cfg(unix)]
        if let Some(mut process) = self.process.take() {
            let exited = process.finish().is_ok();
            self.cleanup_complete = exited && process.cleanup_complete;
            self.status = if exited && process.session_cleanup_complete {
                strings::PTY_STOPPED
            } else {
                strings::PTY_STOP_FAILED
            }
            .into();
            if exited && process.session_cleanup_complete {
                self.status.push_str(&format!("; {}", strings::PTY_STOP_FAILED));
            }
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
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> CommandBuilder {
    let env: Vec<_> = env.into_iter().collect();
    let shell_key = if cfg!(windows) { "ComSpec" } else { "SHELL" };
    let shell = env
        .iter()
        .find(|(k, v)| !v.is_empty() && if cfg!(windows) {
            k.to_string_lossy().eq_ignore_ascii_case(shell_key)
        } else { k == shell_key })
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into());
    let mut cmd = CommandBuilder::new(shell);
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
