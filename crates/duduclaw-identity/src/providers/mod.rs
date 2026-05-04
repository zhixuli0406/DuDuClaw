//! Concrete [`crate::IdentityProvider`] implementations.

pub mod chained;
pub mod notion;
pub mod wiki_cache;

pub use chained::ChainedProvider;
pub use notion::{NotionConfig, NotionFieldMap, NotionIdentityProvider, ProjectsKind};
pub use wiki_cache::WikiCacheIdentityProvider;
