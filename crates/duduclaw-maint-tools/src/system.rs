//! The real [`Executor`]: spawns the fixed programs, talks to the log
//! socket, writes journal entries.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::helper_client::{self, RelayEnd};
use crate::journal::{self, MENU_IDENTIFIER};
use crate::menu::Executor;
use crate::protocol::{HELPER_SOCKET_PATH, LogRequest, RestartTarget};
use crate::sanitize::{PumpEnd, pump_filtered};
use crate::signals;
use crate::tools::{CHILD_ENV, Tool, resolve_tool};

/// Most bytes of a captured stdout kept (only `LoadState` is captured).
const MAX_CAPTURE_BYTES: usize = 4096;

/// Most bytes of a displayed child's output (stdout and stderr together).
pub const MAX_CHILD_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Spawns processes for real. Children get a cleared environment plus
/// [`CHILD_ENV`] and stdin from `/dev/null`; their output is filtered before
/// it reaches the session (see [`Executor::run`] below).
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

    /// The child's stdout and stderr share one pipe that the menu reads and
    /// passes through [`TerminalFilter`](crate::sanitize::TerminalFilter):
    /// part of what these programs print comes from uid 1000 (a unit's
    /// `Status:` text set via `sd_notify`, its description, process names),
    /// so it must not reach the technician's terminal raw. Bounded at
    /// [`MAX_CHILD_OUTPUT_BYTES`]; the child is killed past that.
    fn run(&mut self, program: &Path, args: &[String]) -> io::Result<Option<i32>> {
        io::stdout().flush()?;
        let (mut reader, writer) = io::pipe()?;
        let mut cmd = command(program, args);
        cmd.stdout(writer.try_clone()?).stderr(writer);
        let mut child = cmd.spawn()?;
        // Close our copies of the write end, or the read below never sees EOF.
        drop(cmd);
        let end = {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            pump_filtered(&mut reader, &mut out, MAX_CHILD_OUTPUT_BYTES)
        };
        if !matches!(end, Ok(PumpEnd::Eof)) {
            let _ = child.kill();
        }
        let status = child.wait()?;
        match end? {
            PumpEnd::Eof => Ok(status.code()),
            PumpEnd::Capped => {
                self.warn("\n（輸出過長，已截斷 / Output too long, cut off）\n");
                Ok(status.code())
            }
        }
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
        let end = helper_client::fetch(
            Path::new(HELPER_SOCKET_PATH),
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

    fn request_restart(&mut self, target: RestartTarget) -> io::Result<String> {
        helper_client::request_restart(Path::new(HELPER_SOCKET_PATH), target)
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
