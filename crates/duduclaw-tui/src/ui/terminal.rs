//! A single owner restores raw mode, alternate screen and panic diagnostics.
use crate::{Error, Result, strings};
use crossterm::{cursor, execute, style, terminal};
use std::{
    io::{self, IsTerminal, Write},
    panic::PanicHookInfo,
    sync::atomic::{AtomicBool, Ordering},
};
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PANICKED: AtomicBool = AtomicBool::new(false);
pub fn panicked() -> bool {
    PANICKED.load(Ordering::SeqCst)
}
type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;
pub struct TerminalGuard {
    previous: Option<Hook>,
}
fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        style::ResetColor,
        cursor::Show,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        terminal::LeaveAlternateScreen
    );
    let _ = io::stdout().flush();
}
impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(Error::Terminal);
        }
        if ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Error::Terminal);
        }
        PANICKED.store(false, Ordering::SeqCst);
        let previous = std::panic::take_hook();
        // Never forward a panic payload or location: dependencies may hold secrets.
        std::panic::set_hook(Box::new(|_| {
            PANICKED.store(true, Ordering::SeqCst);
            restore();
            eprintln!("{}", strings::PANIC);
        }));
        let guard = Self {
            previous: Some(previous),
        };
        terminal::enable_raw_mode().map_err(|_| Error::Terminal)?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide,
            crossterm::event::EnableFocusChange, crossterm::event::EnableBracketedPaste)
            .map_err(|_| Error::Terminal)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
        // run_control_room catches unwinding before dropping this guard. The
        // check also keeps Drop safe if an embedding caller itself panics.
        if !std::thread::panicking()
            && let Some(previous) = self.previous.take()
        {
            std::panic::set_hook(previous);
        }
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

pub async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).map_err(|_| Error::Terminal)?;
        let mut hangup = signal(SignalKind::hangup()).map_err(|_| Error::Terminal)?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.map_err(|_| Error::Terminal),
            _ = terminate.recv() => Ok(()),
            _ = hangup.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.map_err(|_| Error::Terminal)
}

#[cfg(all(test, unix))]
#[allow(unsafe_code)] // PTY descriptors and termios are confined to this subprocess test.
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::Read,
        os::fd::{AsRawFd, FromRawFd},
        process::{Command, Stdio},
    };

    #[test]
    fn restores_real_pty_after_success_error_panic_and_cancel() {
        for mode in ["success", "error", "panic", "cancel"] {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: openpty initializes two owned descriptors; null optional
            // arguments request the OS defaults. Neither descriptor is shared.
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(opened, 0, "PTY setup failed");
            // SAFETY: the descriptors were just returned by a successful openpty.
            let mut master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            let mut before = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: valid descriptor and writable termios storage.
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), before.as_mut_ptr()) },
                0
            );
            let before = unsafe { before.assume_init() };
            // Drain concurrently: some PTYs discard queued output on last-slave close.
            let reader = std::thread::spawn(move || {
                let mut output = Vec::new();
                let _ = master.read_to_end(&mut output);
                output
            });
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ui::terminal::tests::probe",
                    "--ignored",
                    "--nocapture",
                ])
                .env("DUDUCLAW_TUI_PTY_PROBE", mode)
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave.try_clone().unwrap()))
                .status()
                .unwrap();
            let mut after = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: same live descriptor and valid termios output storage.
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), after.as_mut_ptr()) },
                0
            );
            let after = unsafe { after.assume_init() };
            // PENDIN is a kernel-maintained pending-retype bit, not a raw-mode setting.
            assert_eq!(
                before.c_lflag & !libc::PENDIN,
                after.c_lflag & !libc::PENDIN,
                "raw mode must be restored: {mode}"
            );
            assert_eq!(before.c_iflag, after.c_iflag);
            assert_eq!(before.c_oflag, after.c_oflag);
            drop(slave);
            let output = reader.join().unwrap();
            let output = String::from_utf8_lossy(&output);
            assert!(status.success(), "PTY child failed: {mode}: {output}");
            assert!(
                output.contains("\x1b[?1049h"),
                "missing enter: {mode}: {output:?}"
            );
            assert!(output.contains("\x1b[?1049l"));
            assert!(!output.contains("secret-canary"));
        }
    }
    #[test]
    #[ignore = "run only by the PTY parent test"]
    fn probe() {
        let mode = std::env::var("DUDUCLAW_TUI_PTY_PROBE").unwrap();
        if mode == "cancel" {
            use std::{
                future::Future,
                task::{Context, Poll},
            };
            let mut future = Box::pin(async {
                let _guard = TerminalGuard::enter().unwrap();
                std::future::pending::<()>().await;
            });
            let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            drop(future);
        } else {
            let result: Result<()> = (|| {
                let guard = TerminalGuard::enter()?;
                if mode == "error" {
                    return Err(Error::Terminal);
                }
                if mode == "panic" {
                    let result = std::panic::catch_unwind(|| panic!("secret-canary"));
                    assert!(result.is_err());
                }
                drop(guard);
                Ok(())
            })();
            assert_eq!(result.is_err(), mode == "error");
        }
        assert!(!terminal::is_raw_mode_enabled().unwrap());
    }
}
