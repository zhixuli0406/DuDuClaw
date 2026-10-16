//! The real [`Executor`]: spawns the fixed programs, talks to the log
//! socket, writes journal entries.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::journal::{self, MENU_IDENTIFIER};
use crate::logs_client::{self, RelayEnd};
use crate::menu::Executor;
use crate::protocol::{LOGS_SOCKET_PATH, LogRequest};
use crate::signals;
use crate::tools::{CHILD_ENV, Tool, resolve_tool};

/// Most bytes of a captured stdout kept (only `LoadState` is captured).
const MAX_CAPTURE_BYTES: usize = 4096;

/// Spawns processes for real. Children get a cleared environment plus
/// [`CHILD_ENV`], stdin from `/dev/null`, and the session's stdout/stderr.
#[derive(Debug, Default)]
pub struct SystemExecutor;

fn command(program: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(CHILD_ENV)
        .stdin(Stdio::null());
    cmd
}

impl Executor for SystemExecutor {
    fn resolve(&self, tool: Tool) -> Option<PathBuf> {
        resolve_tool(tool, |p| p.is_file())
    }

    fn run(&mut self, program: &Path, args: &[String]) -> io::Result<Option<i32>> {
        // Make sure our own buffered text appears before the child's.
        io::stdout().flush()?;
        let status = command(program, args).status()?;
        Ok(status.code())
    }

    fn capture(&mut self, program: &Path, args: &[String]) -> io::Result<String> {
        use std::io::Read;
        let mut child = command(program, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut bytes = Vec::with_capacity(256);
        if let Some(out) = child.stdout.take() {
            // Read at most MAX_CAPTURE_BYTES; dropping the pipe afterwards
            // ends a child that would print more.
            out.take(MAX_CAPTURE_BYTES as u64).read_to_end(&mut bytes)?;
        }
        child.wait()?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn stream_logs(&mut self, req: LogRequest) -> io::Result<()> {
        signals::clear();
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let end = logs_client::fetch(
            Path::new(LOGS_SOCKET_PATH),
            req,
            &mut out,
            &signals::interrupted,
        )?;
        if end == RelayEnd::Interrupted {
            out.write_all("\n（已停止 / Stopped）\n".as_bytes())?;
            out.flush()?;
            signals::clear();
        }
        Ok(())
    }

    fn record(&mut self, priority: u8, message: &str, fields: &[(&str, &str)]) {
        let payload = journal::entry_payload(MENU_IDENTIFIER, priority, message, fields);
        if let Err(e) = journal::send(&payload) {
            // Not fatal for the menu, but never silent.
            self.warn(&format!(
                "（無法寫入操作紀錄 / could not record this action: {e}）\n"
            ));
        }
    }

    fn print(&mut self, text: &str) {
        let mut out = io::stdout().lock();
        // A closed terminal surfaces as a read error on the next prompt.
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }

    fn warn(&mut self, text: &str) {
        let mut err = io::stderr().lock();
        let _ = err.write_all(text.as_bytes()).and_then(|()| err.flush());
    }
}
