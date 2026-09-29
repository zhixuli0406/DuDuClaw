//! [`ChainedProvider`] — combines a fast cache provider with a slower
//! authoritative upstream so deployments get cache-fast hits with
//! upstream-fresh data.
//!
//! ## Semantics
//!
//! - On `resolve_by_channel`: try cache first; on `Ok(None)` (cache miss)
//!   try upstream; if upstream resolves, return upstream record (and
//!   record the source as upstream's name, not "chained"). If upstream is
//!   unreachable / errors, return the cached `Ok(None)` rather than
//!   propagating — agents should treat the sender as a stranger, not get
//!   a hard error.
//! - On `lookup_project_members`: prefer upstream (project membership is
//!   exactly the kind of data that drifts in a cache); on upstream error,
//!   fall back to cache and emit a `tracing::warn!` so operators notice.
//!
//! The cache is *not* automatically refreshed on a hit — write-back is the
//! upstream provider's responsibility (or a separate sync process). This
//! keeps `ChainedProvider` simple and deterministic.

use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

use crate::{ChannelKind, IdentityError, IdentityProvider, ResolvedPerson};

/// Combines a cache provider with an upstream provider.
pub struct ChainedProvider {
    cache: Arc<dyn IdentityProvider>,
    upstream: Arc<dyn IdentityProvider>,
}

impl ChainedProvider {
    pub fn new(cache: Arc<dyn IdentityProvider>, upstream: Arc<dyn IdentityProvider>) -> Self {
        Self { cache, upstream }
    }
}

#[async_trait]
impl IdentityProvider for ChainedProvider {
    async fn resolve_by_channel(
        &self,
        channel: ChannelKind,
        external_id: &str,
    ) -> Result<Option<ResolvedPerson>, IdentityError> {
        // 1. Cache fast path.
        match self.cache.resolve_by_channel(channel.clone(), external_id).await {
            Ok(Some(person)) => return Ok(Some(person)),
            Ok(None) => {} // miss — fall through to upstream
            Err(e) => {
                warn!(
                    cache = self.cache.name(),
                    "ChainedProvider cache error: {} — falling through to upstream",
                    e,
                );
            }
        }

        // 2. Upstream slow path.
        match self.upstream.resolve_by_channel(channel, external_id).await {
            Ok(person) => Ok(person),
            Err(e) => {
                // Soft-fail: agents treat the sender as a stranger rather
                // than seeing a hard error. The provider error is logged
                // so operators can notice upstream outages.
                warn!(
                    upstream = self.upstream.name(),
                    "ChainedProvider upstream error: {} — degrading to no-resolve",
                    e,
                );
                Ok(None)
            }
        }
    }

    async fn lookup_project_members(
        &self,
        project_id: &str,
    ) -> Result<Vec<ResolvedPerson>, IdentityError> {
        // Project membership: prefer upstream because cache is most likely
        // to drift here.
        match self.upstream.lookup_project_members(project_id).await {
            Ok(members) => Ok(members),
            Err(e) => {
                warn!(
                    upstream = self.upstream.name(),
                    "ChainedProvider upstream error in lookup_project_members: {} — falling back to cache",
                    e,
                );
                self.cache.lookup_project_members(project_id).await.map(|members| {
                    info!(
                        cache = self.cache.name(),
                        members = members.len(),
                        "ChainedProvider degraded to cache for project_members"
                    );
                    members
                })
            }
        }
    }

    fn name(&self) -> &str {
        "chained"
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// In-memory provider with controllable failure modes — used to drive
    /// the chained provider through every branch.
    struct FakeProvider {
        name: String,
        people: Vec<ResolvedPerson>,
        // When set, every call returns this error.
        force_error: Mutex<Option<IdentityError>>,
    }

    impl FakeProvider {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                people: vec![],
                force_error: Mutex::new(None),
            }
        }

        fn with_person(mut self, p: ResolvedPerson) -> Self {
            self.people.push(p);
            self
        }

        fn fail_with(self, err: IdentityError) -> Self {
            *self.force_error.lock().unwrap() = Some(err);
            self
        }

        fn current_error(&self) -> Option<IdentityError> {
            self.force_error.lock().unwrap().as_ref().map(clone_err)
        }
    }

    fn clone_err(e: &IdentityError) -> IdentityError {
        match e {
            IdentityError::Unreachable { provider, reason } => {
                IdentityError::Unreachable { provider: provider.clone(), reason: reason.clone() }
            }
            IdentityError::Malformed { provider, reason } => {
                IdentityError::Malformed { provider: provider.clone(), reason: reason.clone() }
            }
            IdentityError::Unsupported { provider, operation } => IdentityError::Unsupported {
                provider: provider.clone(),
                operation: operation.clone(),
            },
            IdentityError::Internal { provider, reason } => {
                IdentityError::Internal { provider: provider.clone(), reason: reason.clone() }
            }
            IdentityError::Io(_) => IdentityError::Internal {
                provider: "fake".into(),
                reason: "io error (cloned)".into(),
            },
        }
    }

    #[async_trait]
    impl IdentityProvider for FakeProvider {
        async fn resolve_by_channel(
            &self,
            channel: ChannelKind,
            external_id: &str,
        ) -> Result<Option<ResolvedPerson>, IdentityError> {
            if let Some(e) = self.current_error() {
                return Err(e);
            }
            let wire = channel.as_wire();
            for p in &self.people {
                if p.channel_handles
                    .get(&wire)
                    .map(|s| s.as_str() == external_id)
                    .unwrap_or(false)
                {
                    return Ok(Some(p.clone()));
                }
            }
            Ok(None)
        }

        async fn lookup_project_members(
            &self,
            project_id: &str,
        ) -> Result<Vec<ResolvedPerson>, IdentityError> {
            if let Some(e) = self.current_error() {
                return Err(e);
            }
            Ok(self
                .people
                .iter()
                .filter(|p| p.project_ids.iter().any(|id| id == project_id))
                .cloned()
                .collect())
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn ruby() -> ResolvedPerson {
        let mut handles = BTreeMap::new();
        handles.insert("discord".into(), "1234567890".into());
        ResolvedPerson {
            person_id: "person_2f9".into(),
            display_name: "Ruby Lin".into(),
            roles: vec![],
            project_ids: vec!["proj-alpha".into()],
            emails: vec![],
            channel_handles: handles,
            source: "fake".into(),
            fetched_at: Utc::now(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_hit_short_circuits_upstream() {
        let cache = Arc::new(FakeProvider::new("cache").with_person(ruby()));
        // Upstream that would error if reached.
        let upstream = Arc::new(
            FakeProvider::new("upstream").fail_with(IdentityError::unreachable("upstream", "down")),
        );
        let chained = ChainedProvider::new(cache, upstream);
        let resolved = chained
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap()
            .expect("cache should hit");
        assert_eq!(resolved.person_id, "person_2f9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_miss_falls_through_to_upstream() {
        let cache = Arc::new(FakeProvider::new("cache"));
        let upstream = Arc::new(FakeProvider::new("upstream").with_person(ruby()));
        let chained = ChainedProvider::new(cache, upstream);
        let resolved = chained
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap()
            .expect("upstream should hit");
        assert_eq!(resolved.person_id, "person_2f9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upstream_unreachable_degrades_to_no_resolve_not_error() {
        let cache = Arc::new(FakeProvider::new("cache"));
        let upstream = Arc::new(
            FakeProvider::new("upstream").fail_with(IdentityError::unreachable("notion", "503")),
        );
        let chained = ChainedProvider::new(cache, upstream);
        // Cache miss + upstream unreachable: must return Ok(None), not Err.
        let resolved = chained
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_error_falls_through_to_upstream() {
        let cache = Arc::new(
            FakeProvider::new("cache").fail_with(IdentityError::Internal {
                provider: "cache".into(),
                reason: "disk full".into(),
            }),
        );
        let upstream = Arc::new(FakeProvider::new("upstream").with_person(ruby()));
        let chained = ChainedProvider::new(cache, upstream);
        let resolved = chained
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap()
            .expect("upstream should hit despite cache error");
        assert_eq!(resolved.person_id, "person_2f9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn project_members_prefer_upstream() {
        let cache = Arc::new(FakeProvider::new("cache").with_person(ruby()));
        let upstream = Arc::new(FakeProvider::new("upstream"));
        // Cache claims one member; upstream is empty. ChainedProvider must
        // return the upstream answer (the authoritative one).
        let chained = ChainedProvider::new(cache, upstream);
        let members = chained.lookup_project_members("proj-alpha").await.unwrap();
        assert!(members.is_empty(), "upstream wins, cache result is ignored");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn project_members_fall_back_to_cache_on_upstream_error() {
        let cache = Arc::new(FakeProvider::new("cache").with_person(ruby()));
        let upstream = Arc::new(
            FakeProvider::new("upstream").fail_with(IdentityError::unreachable("notion", "down")),
        );
        let chained = ChainedProvider::new(cache, upstream);
        let members = chained.lookup_project_members("proj-alpha").await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].person_id, "person_2f9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn name_is_chained() {
        let cache = Arc::new(FakeProvider::new("c"));
        let upstream = Arc::new(FakeProvider::new("u"));
        let chained = ChainedProvider::new(cache, upstream);
        assert_eq!(chained.name(), "chained");
    }

    // ── G5 (2026-09 feature audit) ──────────────────────────────────────────
    // Three properties the module doc promises but nothing pinned down before
    // `ChainedProvider` reached the live resolve paths (channel `<sender>`
    // block + `identity_resolve` MCP tool).

    /// Counting variant of [`FakeProvider`]: proves a call did NOT happen,
    /// which `cache_hit_short_circuits_upstream` could only imply indirectly
    /// (by having the upstream error).
    struct CountingProvider {
        inner: FakeProvider,
        calls: Mutex<usize>,
    }

    impl CountingProvider {
        fn new(name: &str) -> Self {
            Self {
                inner: FakeProvider::new(name),
                calls: Mutex::new(0),
            }
        }
        fn with_person(self, p: ResolvedPerson) -> Self {
            Self {
                inner: self.inner.with_person(p),
                calls: self.calls,
            }
        }
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl IdentityProvider for CountingProvider {
        async fn resolve_by_channel(
            &self,
            channel: ChannelKind,
            external_id: &str,
        ) -> Result<Option<ResolvedPerson>, IdentityError> {
            *self.calls.lock().unwrap() += 1;
            self.inner.resolve_by_channel(channel, external_id).await
        }

        async fn lookup_project_members(
            &self,
            project_id: &str,
        ) -> Result<Vec<ResolvedPerson>, IdentityError> {
            *self.calls.lock().unwrap() += 1;
            self.inner.lookup_project_members(project_id).await
        }

        fn name(&self) -> &str {
            self.inner.name()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_hit_never_touches_the_upstream_at_all() {
        // Stronger than `cache_hit_short_circuits_upstream`: the upstream is
        // not merely tolerant of being skipped, it is never called. This is
        // what keeps a Notion round-trip off every single channel turn.
        let cache = Arc::new(FakeProvider::new("cache").with_person(ruby()));
        let upstream = Arc::new(CountingProvider::new("upstream").with_person(ruby()));
        let chained = ChainedProvider::new(cache, upstream.clone());
        let resolved = chained
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap()
            .expect("cache should hit");
        assert_eq!(resolved.person_id, "person_2f9");
        assert_eq!(upstream.calls(), 0, "a cache hit must not call upstream");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn every_upstream_error_kind_degrades_to_no_resolve() {
        // The degradation contract must not depend on WHICH error the upstream
        // produced — a malformed Notion payload has to be as survivable as a
        // 503, because `build_sender_block` turns `Ok(None)` into "unknown
        // sender" and an `Err` into a logged warning it cannot act on.
        for err in [
            IdentityError::unreachable("notion", "503"),
            IdentityError::Malformed {
                provider: "notion".into(),
                reason: "unexpected property type".into(),
            },
            IdentityError::Unsupported {
                provider: "notion".into(),
                operation: "resolve_by_channel".into(),
            },
            IdentityError::Internal {
                provider: "notion".into(),
                reason: "panic in worker".into(),
            },
        ] {
            let cache = Arc::new(FakeProvider::new("cache"));
            let upstream = Arc::new(FakeProvider::new("upstream").fail_with(err));
            let chained = ChainedProvider::new(cache, upstream);
            let resolved = chained
                .resolve_by_channel(ChannelKind::Discord, "1234567890")
                .await;
            assert!(
                matches!(resolved, Ok(None)),
                "upstream failure must degrade to Ok(None), got {resolved:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_upstream_hit_is_not_written_back_into_the_cache() {
        // Documents the deliberate no-write-back semantics stated in this
        // module's header: `ChainedProvider` is a read path only, so the same
        // external id resolved twice hits the upstream twice. Write-back, if
        // it is ever wanted, belongs to a sync process (or the upstream
        // provider itself) — not to this combinator, which has no write
        // surface on `IdentityProvider` to use.
        let cache = Arc::new(CountingProvider::new("cache")); // empty
        let upstream = Arc::new(CountingProvider::new("upstream").with_person(ruby()));
        let chained = ChainedProvider::new(cache.clone(), upstream.clone());

        for _ in 0..2 {
            let resolved = chained
                .resolve_by_channel(ChannelKind::Discord, "1234567890")
                .await
                .unwrap()
                .expect("upstream should hit");
            assert_eq!(resolved.person_id, "person_2f9");
        }
        assert_eq!(cache.calls(), 2, "cache is consulted on every resolve");
        assert_eq!(
            upstream.calls(),
            2,
            "no write-back: the second resolve still reaches upstream"
        );
    }
}
