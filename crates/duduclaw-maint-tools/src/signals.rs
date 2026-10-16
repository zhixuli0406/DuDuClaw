//! Ctrl-C returns to the menu instead of ending the session.
//!
//! The menu installs a SIGINT *handler* (not `SIG_IGN`): an ignored signal
//! stays ignored across `exec`, so children would become immune to Ctrl-C,
//! while a handler is reset to the default action in every child. Result:
//! Ctrl-C stops `systemctl` or a log follow, and the menu survives and shows
//! itself again (design §5.1).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_signum: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install the SIGINT handler. No `SA_RESTART`, so a blocked socket read
/// returns early; std retries `EINTR` where it matters (line reads, waits,
/// `write_all`).
pub fn install_sigint_handler() -> io::Result<()> {
    // SAFETY: `act` is fully initialised (zeroed, then mask emptied and the
    // handler set) before `sigaction` reads it; `on_sigint` only performs an
    // atomic store, which is async-signal-safe; a null old-action pointer is
    // permitted.
    #[allow(unsafe_code)]
    let rc = unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut act.sa_mask);
        act.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t;
        act.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &act, std::ptr::null_mut())
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether Ctrl-C was pressed since the last [`clear`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Forget an earlier Ctrl-C (called before each action).
pub fn clear() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_sets_and_clear_resets_the_flag() {
        install_sigint_handler().unwrap();
        clear();
        assert!(!interrupted());
        // SAFETY: raising SIGINT in-process runs our handler (installed above).
        #[allow(unsafe_code)]
        let rc = unsafe { libc::raise(libc::SIGINT) };
        assert_eq!(rc, 0);
        assert!(interrupted());
        clear();
        assert!(!interrupted());
    }
}
