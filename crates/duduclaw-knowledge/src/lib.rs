//! Document knowledge base: collections of uploaded documents with
//! collection-level permissions, keyword retrieval and passage lookup.
//!
//! ## Shape
//!
//! - One SQLite file, `<home>/knowledge/knowledge.db` ([`schema`]). A
//!   collection is the unit of permission; every chunk row carries its
//!   collection id so every read filters by the caller's allowed set in SQL.
//! - Permissions ([`acl`]) are default-deny. A grant names a user, an AI
//!   employee, a department or a channel, with a viewer / editor / owner
//!   role. [`acl::effective_collections`] is the single function every read
//!   path calls; anything it cannot read or parse denies.
//! - Document parsing ([`extract`]) only ever runs inside the
//!   `duduclaw knowledge-extract` child process. The gateway owns the worker
//!   that spawns it, the routes and every write.
//! - Retrieval ([`search`]) is keyword-only in this release: FTS5 trigram for
//!   terms of three or more characters, a bounded `LIKE` scan for two-char
//!   terms (「請假」, `HR`), merged with reciprocal rank fusion. The
//!   [`embed::Embedder`] trait is the seam for vector retrieval later; the
//!   only implementation here is [`embed::NoEmbedder`].
//!
//! Document text is data, never instructions: chunks are scanned for
//! injection-like text at index time (`flags` bit 0) and callers render
//! passages as quoted material.

pub mod acl;
pub mod audit;
pub mod blob;
pub mod chunk;
pub mod citations;
pub mod cite;
pub mod embed;
pub mod error;
pub mod eval;
pub mod extract;
pub mod fts;
pub mod index;
pub mod inject;
pub mod jobs;
pub mod limits;
pub mod locator;
pub mod normalize;
pub mod passage;
pub mod rank;
pub mod requester;
pub mod schema;
pub mod search;
pub mod sources;
pub mod store;

pub use error::{KbError, KbResult};

/// Bumped whenever the extractor output for the same bytes can change.
pub const EXTRACTOR_VERSION: &str = "kb-extract/1";
/// Bumped whenever chunk boundaries for the same text can change.
pub const CHUNKER_VERSION: &str = "kb-chunk/1";

/// Current UTC time as an RFC 3339 string with millisecond precision. Every
/// timestamp column uses this format so string order equals time order.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
