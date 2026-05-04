//! The [`IdentityProvider`] trait.

use async_trait::async_trait;

use crate::{ChannelKind, IdentityError, ResolvedPerson};

/// Resolves people from an authoritative identity store.
///
/// All methods are async because production providers (Notion, LDAP, custom
/// HTTP) involve network IO. The [`crate::providers::WikiCacheIdentityProvider`]
/// implementation is purely local but conforms to the same surface so it can
/// be swapped in / out without changing call sites.
///
/// ## Semantics of `Ok(None)`
///
/// `resolve_by_channel` returns `Ok(None)` when the person is unknown. That
/// is the normal "stranger sends a message" case and explicitly **not** an
/// error. Errors are reserved for genuine provider failures (unreachable
/// upstream, malformed payload, IO).
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Resolve a `(channel, external_id)` to its canonical person.
    async fn resolve_by_channel(
        &self,
        channel: ChannelKind,
        external_id: &str,
    ) -> Result<Option<ResolvedPerson>, IdentityError>;

    /// List members of a project. Returns an empty vec for unknown projects;
    /// returns `Err(IdentityError::Unsupported)` for providers that do not
    /// model projects.
    async fn lookup_project_members(
        &self,
        project_id: &str,
    ) -> Result<Vec<ResolvedPerson>, IdentityError>;

    /// Stable identifier for this provider (`"notion"`, `"wiki-cache"`,
    /// `"chained"`, ...). Surfaced into [`ResolvedPerson::source`] and
    /// audit logs.
    fn name(&self) -> &str;
}
