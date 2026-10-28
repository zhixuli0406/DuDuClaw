//! Dependency-light, non-serializable authority for one native OS start intent.
//! This is not a person identity or an attended chat/caller proof.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub home: PathBuf,
    pub agent: String,
    pub key: String,
    pub owner: String,
    pub policy: String,
}
type Revalidate = dyn Fn() -> Result<Binding, String> + Send + Sync;
struct Inner {
    binding: Binding,
    nonce: String,
    revalidate: Arc<Revalidate>,
    expires: Instant,
    claimed: AtomicBool,
    live: AtomicBool,
}
pub(super) struct Permit {
    inner: Arc<Inner>,
}
#[derive(Clone)]
pub(super) struct Authorization {
    inner: Arc<Inner>,
}
pub(super) struct Scope {
    inner: Arc<Inner>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.inner.live.store(false, Ordering::Release);
    }
}
impl Inner {
    fn check(&self, home: &Path, agent: &str) -> Result<(), String> {
        if !self.live.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err("native_start_expired_or_closed".into());
        }
        if self.binding.home != home || self.binding.agent != agent {
            return Err("native_start_context_mismatch".into());
        }
        if (self.revalidate)()? != self.binding {
            return Err("native_start_binding_changed".into());
        }
        Ok(())
    }
}
impl Permit {
    pub(super) fn issue(
        binding: Binding,
        nonce: String,
        revalidate: Arc<Revalidate>,
        ttl: Duration,
    ) -> Result<Self, String> {
        if !binding.home.is_absolute()
            || binding.agent.is_empty()
            || binding.key.is_empty()
            || binding.owner.is_empty()
            || binding.policy.is_empty()
            || nonce.len() != 32
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid_native_start_intent".into());
        }
        Ok(Self {
            inner: Arc::new(Inner {
                binding,
                nonce,
                revalidate,
                expires: Instant::now() + ttl.min(Duration::from_secs(60)),
                claimed: AtomicBool::new(false),
                live: AtomicBool::new(true),
            }),
        })
    }
    pub(super) fn claim(self, home: &Path, agent: &str) -> Result<Authorization, String> {
        self.inner.check(home, agent)?;
        self.inner
            .claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "native_start_already_used")?;
        Ok(Authorization { inner: self.inner })
    }
}
impl Authorization {
    pub(super) fn check(&self, home: &Path, agent: &str) -> Result<(), String> {
        if !self.inner.claimed.load(Ordering::Acquire) {
            return Err("native_start_not_claimed".into());
        }
        self.inner.check(home, agent)
    }
    pub(super) fn scope(&self) -> Scope {
        Scope {
            inner: self.inner.clone(),
        }
    }
    pub(super) fn nonce(&self) -> &str {
        &self.inner.nonce
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;
    fn fixture(ttl: Duration) -> (Permit, Arc<RwLock<Binding>>) {
        let binding = Binding {
            home: PathBuf::from("/synthetic/canonical-home"),
            agent: "alice".into(),
            key: "key-fingerprint".into(),
            owner: "active-owner-epoch".into(),
            policy: "current-policy".into(),
        };
        let current = Arc::new(RwLock::new(binding.clone()));
        let check = current.clone();
        let permit = Permit::issue(
            binding,
            "a1234567890123456789012345678901".into(),
            Arc::new(move || Ok(check.read().unwrap().clone())),
            ttl,
        )
        .unwrap();
        (permit, current)
    }
    #[test]
    fn a_native_intent_is_single_use_and_does_not_cross_home_or_employee() {
        let (permit, _) = fixture(Duration::from_secs(60));
        let replay = Permit {
            inner: permit.inner.clone(),
        };
        assert!(
            replay
                .inner
                .check(Path::new("/another/home"), "alice")
                .is_err()
        );
        assert!(
            replay
                .inner
                .check(Path::new("/synthetic/canonical-home"), "bob")
                .is_err()
        );
        let grant = permit
            .claim(Path::new("/synthetic/canonical-home"), "alice")
            .unwrap();
        let _scope = grant.scope();
        assert!(
            grant
                .check(Path::new("/synthetic/canonical-home"), "alice")
                .is_ok()
        );
        assert!(
            replay
                .claim(Path::new("/synthetic/canonical-home"), "alice")
                .is_err()
        );
    }
    #[test]
    fn changed_key_policy_or_owner_binding_refuses_an_already_claimed_intent() {
        for field in ["key", "policy", "owner"] {
            let (permit, current) = fixture(Duration::from_secs(60));
            let grant = permit
                .claim(Path::new("/synthetic/canonical-home"), "alice")
                .unwrap();
            let _scope = grant.scope();
            let mut binding = current.write().unwrap();
            match field {
                "key" => binding.key = "rotated".into(),
                "policy" => binding.policy = "revoked".into(),
                _ => binding.owner = "owner-changed-or-suspended".into(),
            };
            drop(binding);
            assert!(
                grant
                    .check(Path::new("/synthetic/canonical-home"), "alice")
                    .is_err()
            );
        }
    }
    #[test]
    fn an_expired_intent_never_claims_even_with_identical_current_bindings() {
        let (permit, _) = fixture(Duration::ZERO);
        assert!(
            permit
                .claim(Path::new("/synthetic/canonical-home"), "alice")
                .is_err()
        );
    }
    #[test]
    fn dropping_the_host_scope_revokes_every_cloned_start_authorization() {
        let (permit, _) = fixture(Duration::from_secs(60));
        let grant = permit
            .claim(Path::new("/synthetic/canonical-home"), "alice")
            .unwrap();
        let clone = grant.clone();
        let scope = grant.scope();
        drop(scope);
        assert!(
            clone
                .check(Path::new("/synthetic/canonical-home"), "alice")
                .is_err()
        );
    }
}
