//! Synchronous protocol compatibility over the same execution backend.
//! The runtime remains alive while blocking clients own stream/run handles.
use std::{future::Future, io::{self, Read, Write}, process::{Command, ExitStatus}, sync::OnceLock};
use crate::execution_gate::{self, ExecutionClass, RunControl};

fn runtime() -> io::Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<io::Result<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1).enable_all().thread_name("execution-blocking").build())
        .as_ref().map_err(|error| io::Error::other(error.to_string()))
}

fn wait<F: Future + Send>(future: F) -> io::Result<F::Output>
where F::Output: Send {
    let runtime = runtime()?;
    // A blocking protocol can also be called inside a current-thread executor.
    // Enter the dedicated runtime on another thread rather than nesting block_on.
    std::thread::scope(|scope| scope.spawn(|| runtime.block_on(future)).join())
        .map_err(|_| io::Error::other("execution worker failed"))
}

pub struct Reader(execution_gate::ChildStdout);
impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        use tokio::io::AsyncReadExt;
        wait(self.0.read(buffer))?
    }
}
pub struct Writer(execution_gate::ChildStdin);
impl Write for Writer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        use tokio::io::AsyncWriteExt;
        wait(self.0.write(buffer))?
    }
    fn flush(&mut self) -> io::Result<()> {
        use tokio::io::AsyncWriteExt;
        wait(self.0.flush())?
    }
}

pub struct Child {
    inner: execution_gate::Child,
    pub stdin: Option<Writer>,
    pub stdout: Option<Reader>,
    pub stderr: Option<Reader>,
}
impl Child {
    pub fn id(&self) -> Option<u32> { self.inner.id() }
    pub fn control(&self) -> RunControl { self.inner.control() }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> { self.inner.try_wait() }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        wait(self.inner.wait())?
    }
    pub fn kill(&mut self) -> io::Result<()> { wait(self.inner.kill())? }
}

pub fn spawn(command: &mut Command, class: ExecutionClass) -> io::Result<Child> {
    let captured = execution_gate::CapturedLaunch::capture();
    let native = std::mem::replace(command, Command::new(""));
    let mut command = tokio::process::Command::from(native);
    let mut child = wait(captured.scope(execution_gate::spawn(&mut command, class)))??;
    Ok(Child { stdin: child.stdin.take().map(Writer), stdout: child.stdout.take().map(Reader),
        stderr: child.stderr.take().map(Reader), inner: child })
}

/// Capture both outputs with closed stdin, matching our synchronous output
/// callers. Commands with explicit stdio redirection use spawn/status instead.
pub fn output(command: &mut Command, class: ExecutionClass) -> io::Result<std::process::Output> {
    let captured = execution_gate::CapturedLaunch::capture();
    command.stdin(std::process::Stdio::null());
    let native = std::mem::replace(command, Command::new(""));
    let mut command = tokio::process::Command::from(native);
    wait(captured.scope(execution_gate::output(&mut command, class)))?
}

pub fn status(command: &mut Command, class: ExecutionClass) -> io::Result<ExitStatus> {
    spawn(command, class)?.wait()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn blocking_backend_preserves_bytes_without_nested_runtime() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '%s' \"$VALUE\"; printf err >&2"])
            .env_clear().env("VALUE", "繁體中文 $HOME\n");
        let output = output(&mut command, ExecutionClass::Discovery).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, "繁體中文 $HOME\n".as_bytes());
        assert_eq!(output.stderr, b"err");
    }
}
