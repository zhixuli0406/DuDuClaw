//! duduclaw-redaction — RFC-23: Sensitive Data Redaction Pipeline.
//!
//! Provides source-aware redaction of internal data (Odoo / shared wiki /
//! file tool results) before it reaches the online LLM, with reversible
//! restoration at trusted boundaries (user channel reply, whitelisted tool
//! egress).
//!
//! See [`commercial/docs/RFC-23-redaction-pipeline.md`] for the full design.
//!
//! ## High-level flow
//!
//! ```text
//! Tool result (sensitive) ──redact──► <REDACT:CAT:hash> ──► LLM context
//!                                                              │
//!                                                              ▼
//!                                                       LLM response
//!                                            ┌─────────────────┴─────────────────┐
//!                                            ▼                                   ▼
//!                                  Channel reply (restore)            Tool call args (egress)
//!                                                                 (whitelisted: restore; else: deny)
//! ```
//!
//! ## Crate layout
//!
//! - [`token`]  — token type + per-session HMAC hash
//! - [`source`] — `Source`, `Caller`, `RestoreTarget`
//! - [`rules`]  — `Rule` trait + matchers (regex, identity, ...)
//! - [`engine`] — rule set + conflict resolution
//! - [`vault`]  — encrypted SQLite mapping store
//! - [`pipeline`] — top-level redact / restore API
//! - [`config`] — `RedactionConfig`, `Profile`
//! - [`custom_rules`] — pure helpers for dashboard-authored rules
//! - [`data_source`] — data-source registry (`db_field` tool bindings)
//! - [`egress`] — tool egress whitelist + arg restoration
//! - [`audit`]  — JSONL audit sink
//! - [`profiles`] — embedded built-in profiles
//! - [`locate`] — pointer/token location helpers for evidence reports
//! - [`ner`] — local NER model ("AI 智慧偵測"): manifest, install, runtime

pub mod audit;
pub mod config;
pub mod custom_rules;
pub mod dashboard;
pub mod data_source;
pub mod egress;
pub mod engine;
pub mod error;
pub mod gc;
pub mod locate;
pub mod manager;
pub mod ner;
pub mod pipeline;
pub mod profiles;
pub mod rules;
pub mod source;
pub mod toggle;
pub mod token;
pub mod vault;

pub use audit::{AuditEvent, AuditSink, JsonlAuditSink, NullAuditSink};
pub use config::{
    Profile, ProfileMeta, RedactionConfig, RestoreArgsMode, SourceMode, SourcePolicy, ToolEgressRule,
};
pub use custom_rules::{
    build_pattern, derive_category_id, is_valid_category, matches_anywhere, matches_fully,
    pattern_satisfies, suggest_pattern_heuristic, synthesize_example,
};
pub use data_source::{
    BUILTIN_SOURCE_NAMES, DataSource, DataSourceDef, TableSource, ToolBinding, builtin_sources,
    is_builtin_source, is_valid_data_source_name,
};
pub use egress::{EgressDecision, EgressEvaluator};
pub use engine::{EngineOptions, MatchedSpan, RuleEngine, StructuredHit};
pub use error::{RedactionError, Result};
pub use gc::{GcConfig, GcTask, spawn_gc};
pub use locate::{collect_token_locations, escape_pointer, scan_tokens};
pub use manager::{ManagerPaths, RedactionManager, resolve_data_sources, resolve_rule_specs};
pub use ner::{InstallDirs, NerConfig};
pub use pipeline::{RedactionOutput, RedactionPipeline, ToolContext};
pub use rules::{IdentityRule, JsonPathRule, Match, RestoreScope, Rule, RuleKind, RuleSpec};
pub use source::{Caller, RestoreTarget, Source};
pub use toggle::{
    ChannelPolicy, CliFlag, EnvSetting, ForceOverrideFlag, ForceOverrideRecord, ToggleDecision,
    ToggleInputs, ToggleReason, compute_effective_enabled, override_banner,
};
pub use token::Token;
pub use vault::{VaultEntry, VaultStats, VaultStore};
