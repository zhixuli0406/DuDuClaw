//! duduclaw-identity — RFC-21 §1: Identity Resolution provider abstraction.
//!
//! Resolves a `(channel, external_id)` pair (e.g. a Discord `user_id`) to the
//! canonical person it represents, behind an [`IdentityProvider`] trait so
//! operators can plug in Notion / LDAP / a custom HTTP source as the
//! authoritative identity store. The shared wiki at
//! `~/.duduclaw/shared/wiki/identity/people/` may serve as a *cache* layer
//! ([`WikiCacheIdentityProvider`]) but is no longer the source of truth.
//!
//! ## Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use duduclaw_identity::{ChannelKind, IdentityProvider, providers::WikiCacheIdentityProvider};
//!
//! # async fn demo(home_dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
//! let provider = WikiCacheIdentityProvider::for_home(home_dir);
//! let resolved = provider.resolve_by_channel(ChannelKind::Discord, "1234567890").await?;
//! if let Some(person) = resolved {
//!     println!("Discord user belongs to {} ({})", person.display_name, person.person_id);
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod principal;
pub mod provider;
pub mod providers;

pub use error::IdentityError;
pub use principal::{ChannelKind, ResolvedPerson};
pub use provider::IdentityProvider;
