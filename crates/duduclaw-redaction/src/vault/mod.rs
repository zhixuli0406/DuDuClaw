//! Encrypted token ↔ original-value vault.
//!
//! See [`store::VaultStore`] for the public API. `schema` and `key` are
//! internal but exposed so the binary can run `vault init` or rotate keys
//! from a CLI if needed.

pub mod key;
pub mod schema;
pub mod store;

pub use store::{VaultEntry, VaultStats, VaultStore};
