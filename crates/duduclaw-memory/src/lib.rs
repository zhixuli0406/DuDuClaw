pub mod decay;
pub mod embedding;
pub mod engine;
pub mod feedback;
pub mod import;
pub mod janitor;
pub mod router;
pub mod search;
pub mod trust_store;
pub mod wiki;

pub use embedding::VectorIndex;
pub use engine::{KeyFact, SqliteMemoryEngine, word_jaccard};
pub use feedback::{CitationTracker, DrainOnDrop, TrustSignal, WikiCitation};
pub use janitor::{JanitorConfig, JanitorReport, WikiJanitor};
pub use router::classify;
pub use trust_store::{TrustUpdateOutcome, UpsertResult, WikiTrustSnapshot, WikiTrustStore};
pub use wiki::{SourceType, WikiFts, WikiLayer, WikiStore};
