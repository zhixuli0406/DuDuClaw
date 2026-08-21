//! CD-2 (task brief item 1): socket-auth token rotation WITHOUT restarting
//! `duduclaw-comp`. Split out of `mod.rs` purely for file size (that module
//! was already near this project's per-file convention — 200-400 lines
//! typical, 800 max — before this round); every type/field this needs
//! (`CodriveShared`, its private `auth_token`/`token_path` fields, and the
//! free functions `generate_token_bytes`/`hex_encode`/`write_token_file`)
//! stays private to the parent `codrive` module and is visible here because
//! this is one of its child modules — no visibility was widened to make
//! this split possible.
//!
//! Two independent triggers, both routed through [`CodriveShared::
//! rotate_token`] below: an authenticated connection sending
//! `{"op":"rotate_token"}` (`listener.rs`), or this process receiving
//! `SIGHUP` ([`block_sighup_on_current_thread`] + [`spawn_sighup_rotation_thread`]).
//! See `BUILD.md`'s "CD-2 socket token rotation" section for the live
//! (real-signal, real-process) verification this pattern was checked
//! against — getting the masking order wrong is the actual danger with
//! SIGHUP (its default disposition is to terminate the process), so this
//! was not shipped on unit tests alone.

use std::sync::Arc;

use super::CodriveShared;

impl CodriveShared {
    /// Rotates this run's socket-auth token: generates a fresh 32-byte
    /// token via the exact same path `init` used at startup
    /// (`generate_token_bytes` → `hex_encode` → `write_token_file`), then
    /// swaps the in-memory value `check_token` reads under `auth_token`'s
    /// mutex. `trigger` is stamped on the `token_rotated` audit line
    /// (`"socket_op"` / `"sighup"`) purely for operator visibility — it
    /// plays no role in authorization.
    ///
    /// **Old token invalidated immediately; already-authenticated
    /// connections are unaffected.** `check_token` is consulted exactly
    /// once per connection, inside `listener.rs::authenticate`, before
    /// anything else happens on that connection. Once a connection is past
    /// that gate it never calls `check_token` again for the rest of its
    /// life — so the very next NEW connection attempt must present the
    /// just-rotated token, while any connection already mid-session
    /// (including the one that may have just requested this rotation over
    /// the socket) keeps running exactly as before. This falls out of
    /// `authenticate()`'s existing structure; this method does not need to
    /// track or notify existing connections at all.
    ///
    /// Fails closed: if either the random-byte read or the file write
    /// fails, this returns `Err` and the mutex is never touched — the OLD
    /// token (if any) stays in effect rather than `auth_token` ending up
    /// cleared or half-written. Also fails, before ever touching
    /// `/dev/urandom`, if this run has no token file path at all (`init`'s
    /// fail-closed disabled paths — the listener was never started), since
    /// rotating a token nothing could ever have been authenticated with in
    /// the first place would be pointless.
    pub(crate) fn rotate_token(&self, trigger: &'static str) -> std::io::Result<()> {
        let Some(token_path) = self.token_path.as_deref() else {
            return Err(std::io::Error::other(
                "codrive: cannot rotate — no token file path for this run (listener never started)",
            ));
        };
        let bytes = super::generate_token_bytes()?;
        let new_token = super::hex_encode(&bytes);
        super::write_token_file(token_path, &new_token)?;

        match self.auth_token.lock() {
            Ok(mut guard) => *guard = Some(new_token),
            Err(poisoned) => *poisoned.into_inner() = Some(new_token),
        }
        self.record("token_rotated", Some(trigger), None, None, None);
        Ok(())
    }
}

/// Blocks `SIGHUP` for the CALLING thread (CD-2 task item 1). POSIX
/// `pthread_sigmask` masks per-thread, not process-wide — and a new thread
/// inherits its creator's mask at `pthread_create` time — so this MUST run
/// before any other thread in this process is spawned. `codrive::init` is
/// called from `DuduclawComp::new`, which itself runs before `main.rs`
/// calls `winit_backend::init_winit` (the first thing in this binary that
/// spawns worker threads of its own — EGL/GL context setup, the wayland
/// event dispatch loop, etc.); nothing before `init` runs in this process's
/// startup sequence spawns a thread either (seat/xdg/output state setup is
/// all in-process bookkeeping). Calling this as the very first statement of
/// `init`, before even creating the agent seat, is therefore sufficient —
/// every thread spawned afterward (the injection-socket listener, the
/// SIGHUP-wait thread itself, anything winit later spawns) inherits the
/// blocked mask automatically; no thread but this one ever needs to call
/// this function.
///
/// Why this matters: SIGHUP's default disposition is "terminate the
/// process". If SIGHUP is blocked on only SOME threads, the kernel can
/// still deliver it to any thread that does not block it, running that
/// thread's default disposition there — i.e. it kills the whole compositor
/// instead of ever reaching the dedicated `sigwait` thread this module
/// spawns to turn SIGHUP into a token rotation. This is the standard,
/// documented "block on every thread up front, dequeue synchronously on one
/// dedicated thread" pattern for handling a signal outside of a real
/// `signal()`/`sigaction()` handler (which would otherwise have to obey
/// async-signal-safety rules — no allocation, no mutex locking, no
/// `tracing` calls — that `rotate_token`'s file I/O and mutex use cannot
/// satisfy).
pub(super) fn block_sighup_on_current_thread() -> std::io::Result<()> {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGHUP);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Spawns the dedicated thread that turns a blocked `SIGHUP` into a token
/// rotation (CD-2 task item 1's "支援 SIGHUP...觸發重產"). Requires
/// `block_sighup_on_current_thread` to have already succeeded on the
/// process's main thread — see that function's doc; only called from
/// `init`'s success path, after that precondition is confirmed. `sigwait`
/// synchronously and safely dequeues a blocked-but-pending signal, so the
/// body of this loop runs as ordinary code (free to log, lock a mutex, do
/// file I/O) instead of inside signal-handler context.
pub(super) fn spawn_sighup_rotation_thread(shared: Arc<CodriveShared>) {
    let spawned = std::thread::Builder::new()
        .name("codrive-sighup".into())
        .spawn(move || {
            let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGHUP);
            }
            loop {
                let mut sig: libc::c_int = 0;
                let rc = unsafe { libc::sigwait(&set, &mut sig) };
                if rc != 0 {
                    tracing::warn!(
                        rc,
                        "codrive: sigwait failed — SIGHUP-triggered token rotation is no \
                         longer available this run (socket-op rotation is unaffected)"
                    );
                    return;
                }
                tracing::info!("codrive: SIGHUP received — rotating the injection-socket auth token");
                if let Err(e) = shared.rotate_token("sighup") {
                    tracing::error!(error = %e, "codrive: SIGHUP-triggered token rotation failed");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!(
            error = %e,
            "codrive: failed to spawn the SIGHUP token-rotation thread — SIGHUP-triggered \
             rotation is unavailable this run (socket-op rotation is unaffected)"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn rotate_test_token_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "duduclaw-codrive-test-rotate-unit-{label}-{}-{}.token",
            std::process::id(),
            uuid_ish()
        ))
    }

    /// Tiny process-local uniqueness helper — this crate has no `uuid`
    /// dependency (unlike the gateway crate's tests), and these unit tests
    /// only need "won't collide with a sibling test run in this same
    /// process", not a real UUID.
    fn uuid_ish() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn rotate_token_swaps_check_token_and_rewrites_the_file() {
        let path = rotate_test_token_path("swap");
        let _ = std::fs::remove_file(&path);
        let shared = CodriveShared::for_test_with_token_path(Some("old".to_string()), path.clone());

        assert!(shared.check_token("old"));
        shared.rotate_token("test").expect("rotate must succeed");
        assert!(!shared.check_token("old"), "old token must stop authenticating immediately");

        let written = std::fs::read_to_string(&path).expect("rotate must have written the token file");
        assert_ne!(written, "old", "the file must hold the NEW token, not the old one");
        assert_eq!(written.len(), 64, "the new token must be 64 hex chars (32 bytes)");
        assert!(shared.check_token(&written), "the freshly written token must now authenticate");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_token_two_rotations_produce_different_tokens() {
        let path = rotate_test_token_path("twice");
        let _ = std::fs::remove_file(&path);
        let shared = CodriveShared::for_test_with_token_path(Some("start".to_string()), path.clone());

        shared.rotate_token("test").expect("first rotate must succeed");
        let first = std::fs::read_to_string(&path).unwrap();
        shared.rotate_token("test").expect("second rotate must succeed");
        let second = std::fs::read_to_string(&path).unwrap();

        assert_ne!(first, second, "two rotations must not produce the same token");
        assert!(!shared.check_token(&first), "the token from the first rotation must no longer work");
        assert!(shared.check_token(&second));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_token_fails_closed_without_a_token_path() {
        // `for_test` (no path) mirrors `init`'s fail-closed disabled state
        // (token generation/write failed at startup — see `disabled_keep_
        // audit`) — rotation must refuse rather than panic or silently
        // no-op.
        let shared = CodriveShared::for_test(Some("old".to_string()));
        let err = shared.rotate_token("test").expect_err("rotation with no token path must fail");
        assert!(err.to_string().contains("no token file path"));
        // And must not have touched the existing token.
        assert!(shared.check_token("old"));
    }
}
