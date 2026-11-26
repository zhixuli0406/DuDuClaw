//! One-use host capability passed explicitly to the native spawn backend.
use std::{io, sync::{Arc, Mutex}, time::Instant};
use crate::process_identity::{ProcessIdentity, RootProcess};

/// Shared policy limit for the phase-2 peer ancestry verifier.
pub const MAX_ANCESTRY_DEPTH: usize = 64;

#[derive(Debug)]
enum State { Pending, Bound(RootProcess), Revoked }

#[derive(Debug)]
struct Inner { state: State, deadline: Instant }

/// Clones refer to the same lifecycle, never to a fresh binding. This capability
/// carries no token and cannot be recovered from an environment variable.
#[derive(Debug, Clone)]
pub struct SpawnBinding(Arc<Mutex<Inner>>);

impl SpawnBinding {
    pub fn pending(deadline: Instant) -> Self {
        Self(Arc::new(Mutex::new(Inner { state: State::Pending, deadline })))
    }

    /// Only trusted spawn code calls this, before starting any background reaper.
    /// An attempted second bind revokes the original capability as well.
    pub fn bind_spawn(&self, root: RootProcess) -> io::Result<()> {
        let mut inner = self.0.lock().map_err(|_| denied())?;
        if !matches!(inner.state, State::Pending) || Instant::now() >= inner.deadline || !root.is_current() {
            inner.state = State::Revoked;
            return Err(denied());
        }
        inner.state = State::Bound(root);
        Ok(())
    }

    /// The IPC handler must additionally authenticate the peer and walk its ancestry.
    /// A bound root alone is not proof that a bearer is that root's descendant.
    pub fn root(&self) -> Option<ProcessIdentity> {
        let mut inner = self.0.lock().ok()?;
        if Instant::now() < inner.deadline {
            match &inner.state {
                State::Bound(root) if root.is_current() => return Some(root.identity()),
                State::Pending => return None,
                _ => {}
            }
        }
        inner.state = State::Revoked;
        None
    }

    pub fn revoke(&self) {
        if let Ok(mut inner) = self.0.lock() { inner.state = State::Revoked; }
    }
}

fn denied() -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, "run process binding refused") }

/// Backend lifetime guard: revoke at root exit/cancel, before output EOF, and
/// also if the supervisor future unwinds or is dropped.
pub(crate) struct RevokeOnDrop(pub Option<SpawnBinding>);
impl Drop for RevokeOnDrop {
    fn drop(&mut self) { if let Some(binding) = &self.0 { binding.revoke(); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending() -> SpawnBinding { SpawnBinding::pending(Instant::now() + std::time::Duration::from_secs(60)) }

    #[test]
    fn pending_refuses_and_binding_is_single_use() {
        let binding = pending();
        assert!(binding.root().is_none());
        binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).unwrap();
        assert_eq!(binding.root().unwrap().pid, std::process::id());
        assert!(binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).is_err());
        assert!(binding.root().is_none());
    }

    #[test]
    fn cancellation_and_expiry_revoke() {
        let binding = pending();
        binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).unwrap();
        drop(RevokeOnDrop(Some(binding.clone())));
        assert!(binding.root().is_none());
        let binding = SpawnBinding::pending(Instant::now());
        assert!(binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).is_err());
    }

    #[test]
    fn root_fixture() {
        if std::env::var_os("DUDUCLAW_BINDING_ROOT_FIXTURE").is_none() { return; }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }

    #[test]
    fn root_exit_revokes_even_while_guard_is_retained() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "spawn_binding::tests::root_fixture", "--nocapture"])
            .env("DUDUCLAW_BINDING_ROOT_FIXTURE", "1")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).spawn().unwrap();
        let binding = pending();
        binding.bind_spawn(RootProcess::capture(child.id()).unwrap()).unwrap();
        assert!(binding.root().is_some());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(binding.root().is_none());
        assert!(binding.bind_spawn(RootProcess::capture(std::process::id()).unwrap()).is_err());
    }
}
