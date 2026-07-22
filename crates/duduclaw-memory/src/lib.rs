pub mod bench;
pub mod code_map;
pub mod decay;
pub mod embedding;
pub mod engine;
pub mod feedback;
pub mod gdpr;
pub mod graph_rank;
pub mod import;
pub mod janitor;
pub mod lifecycle;
pub mod origin;
pub mod router;
pub mod search;
pub mod trust_store;
pub mod user_code;
pub mod user_profile;
pub mod vector;
pub mod wiki;

pub use bench::{graph_rank_bench, GraphBenchReport};
pub use code_map::{CodeMap, CodeMapConfig, RankedFile, SymbolInfo, SymbolKind};
pub use embedding::VectorIndex;
pub use vector::{EmbeddingProvider, NgramHashEmbedder};
pub use engine::{
    DecisionResolveOutcome, DecisionView, KeyFact, SqliteMemoryEngine, TemporalMeta,
    TemporalRecord, word_jaccard,
};
pub use feedback::{CitationTracker, DrainOnDrop, TrustSignal, WikiCitation};
pub use gdpr::{gdpr_erase, gdpr_export, GdprEraseSummary};
pub use janitor::{JanitorConfig, JanitorReport, WikiJanitor};
pub use lifecycle::{reassign_agent, reassign_agent_cross_db, ReassignSummary};
pub use origin::{trust_ceiling, OriginClass};
pub use router::classify;
pub use trust_store::{TrustUpdateOutcome, UpsertResult, WikiTrustSnapshot, WikiTrustStore};
pub use user_code::{
    compile_user_profile, ActionDescriptor, Condition, Conflict, Polarity, Provenance, RuleHit,
    UserProfile, UserRule,
};
pub use user_profile::{
    consolidate_profile, profile_block, profile_traits, record_trait, ProfileTrait,
};
pub use wiki::{SourceType, WikiFts, WikiLayer, WikiStore};

// ── Night Engine (N3/N4 deterministic memory passes) ──
pub mod night;
pub use night::{
    consolidate_recurrent, detect_themes, induce_schema, recurrence_gate,
    verify_consolidation, ConsolidationResult, InducedSchema, Theme, VerificationReport,
};
