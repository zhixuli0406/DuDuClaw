pub mod bench;
pub mod code_map;
pub mod causal;
pub mod causal_alias;
pub mod causal_extract;
pub mod causal_eval;
pub mod causal_model;
pub mod causal_effect;
pub mod causal_effect_eval;
pub mod causal_identify;
pub mod causal_negative_control;
pub mod causal_revision;
pub mod causal_memory;
pub mod causal_wiki;
pub mod decay;
pub mod embedding;
pub mod engine;
pub mod feedback;
pub mod gdpr;
pub mod graph_rank;
pub mod import;
pub mod janitor;
pub mod lineage;
pub mod lifecycle;
pub mod novelty_gate;
pub mod origin;
pub mod router;
pub mod sensitivity;
pub mod supersession_guard;
pub mod trust_store;
pub mod user_code;
pub mod user_profile;
pub mod vector;
pub mod wiki;
pub mod wiki_fence;

pub use bench::{graph_rank_bench, GraphBenchReport};
pub use code_map::{CodeMap, CodeMapConfig, RankedFile, SymbolInfo, SymbolKind};
pub use vector::{EmbeddingProvider, NgramHashEmbedder};
pub use engine::{
    is_system_signal, DecisionResolveOutcome, DecisionView, KeyFact, MigrationDisposition,
    NamespaceRow, OnRefused, SqliteMemoryEngine,
    TemporalMeta, TemporalRecord, word_jaccard, PREDICTION_CONTENT_PREFIX,
    SYSTEM_SIGNAL_SOURCE_EVENTS,
};
pub use feedback::{CitationTracker, DrainOnDrop, TrustSignal, WikiCitation};
pub use gdpr::{gdpr_erase, gdpr_export, GdprEraseSummary};
pub use janitor::{JanitorConfig, JanitorReport, WikiJanitor};
pub use lifecycle::{reassign_agent, reassign_agent_cross_db, ReassignSummary};
// `Provenance` stays under `lineage::` — the crate root already exports
// `user_code::Provenance` (a different type).
pub use lineage::{
    format_ts, source_digest, FactWriteOutcome, FenceReason, FenceRefusal, SourceKind, SourceRef,
    MAX_LINEAGE_SOURCES,
};
pub use engine::forget_source::{
    ApplyOutcome, ApplyReport, ExternalInputs, ForgetPlan, ForgetSelector, ForgetStep,
    PlanDocument, PlanOptions, PlanOutcome, SessionMessageRef, StaleReason, WikiPageRef,
};
pub use novelty_gate::{NoveltyGateConfig, NoveltyRejection};
pub use origin::{trust_ceiling, OriginClass};
pub use supersession_guard::{
    claim_digest, HeldClaim, HeldClaimView, PromotionReport, ReleaseHeld, ReleaseReport,
    SupersessionRefusal,
    TemporalWriteOutcome,
};
pub use router::classify;
pub use sensitivity::{
    read_from_metadata as read_sensitivity_metadata, stamp_metadata as stamp_sensitivity_metadata,
};
pub use trust_store::{TrustUpdateOutcome, UpsertResult, WikiTrustSnapshot, WikiTrustStore};
pub use user_code::{
    compile_user_profile, ActionDescriptor, Condition, Conflict, Polarity, Provenance, RuleHit,
    UserProfile, UserRule,
};
pub use user_profile::{
    consolidate_profile, profile_block, profile_traits, record_trait, ProfileTrait,
};
pub use wiki::{serialize_page, SourceType, WikiFts, WikiLayer, WikiPage, WikiStore};
pub use wiki_fence::{
    WikiDeliveryFence, WikiDeliveryLease, WikiFenceError, WikiMutationGuard, is_fence_busy,
};

// ── Night Engine (N3/N4 deterministic memory passes) ──
pub mod night;
pub use night::{
    consolidate_recurrent, detect_themes, induce_schema, recurrence_gate,
    verify_consolidation, ConsolidationResult, InducedSchema, Theme, VerificationReport,
};
