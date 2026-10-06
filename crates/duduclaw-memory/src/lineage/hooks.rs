//! Test-only pause/fault points for the publish and forget-apply paths.
//!
//! Compiled to no-ops unless the crate is built for its own tests or with the
//! `test-hooks` feature (a dev-dependency feature for other crates' tests).
//! Race tests use these with `std::sync::Barrier` instead of sleeping.

use duduclaw_core::error::Result;

/// A point in a temporal / fact write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// Before the write's transaction begins (nothing locked yet).
    BeforeTxn,
    /// Inside the write's transaction, right after the source fence check.
    AfterFenceCheck,
}

/// A point in `apply_forget_plan`. A hook returning `Err` at any point inside
/// the transaction makes the apply roll back (SQL fault injection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyHookPoint {
    /// Before `BEGIN IMMEDIATE` (the apply may block on a writer after this).
    BeforeBegin,
    AfterTombstones,
    AfterDeletes,
    BeforeCommit,
    /// After `COMMIT`, before any post-commit housekeeping.
    AfterCommit,
}

#[cfg(any(test, feature = "test-hooks"))]
pub type PublishHook = std::sync::Arc<dyn Fn(HookPoint) + Send + Sync>;
#[cfg(any(test, feature = "test-hooks"))]
pub type ApplyHook = std::sync::Arc<dyn Fn(ApplyHookPoint) -> Result<()> + Send + Sync>;

/// Hook slots held by the engine. Zero-sized when hooks are compiled out.
#[derive(Default)]
pub struct TestHooks {
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) publish: std::sync::RwLock<Option<PublishHook>>,
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) apply: std::sync::RwLock<Option<ApplyHook>>,
}

impl TestHooks {
    #[allow(unused_variables)]
    pub(crate) fn fire_publish(&self, point: HookPoint) {
        #[cfg(any(test, feature = "test-hooks"))]
        {
            let hook = self.publish.read().ok().and_then(|g| g.clone());
            if let Some(h) = hook {
                h(point);
            }
        }
    }

    #[allow(unused_variables)]
    pub(crate) fn fire_apply(&self, point: ApplyHookPoint) -> Result<()> {
        #[cfg(any(test, feature = "test-hooks"))]
        {
            let hook = self.apply.read().ok().and_then(|g| g.clone());
            if let Some(h) = hook {
                return h(point);
            }
        }
        Ok(())
    }
}
