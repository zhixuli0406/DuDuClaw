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
//! - [`egress`] — tool egress whitelist + arg restoration
//! - [`audit`]  — JSONL audit sink
//! - [`profiles`] — embedded built-in profiles

pub mod audit;
pub mod config;
pub mod dashboard;
pub mod egress;
pub mod engine;
pub mod error;
pub mod gc;
pub mod manager;
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
pub use egress::{EgressDecision, EgressEvaluator};
pub use engine::{MatchedSpan, RuleEngine};
pub use error::{RedactionError, Result};
pub use gc::{GcConfig, GcTask, spawn_gc};
pub use manager::{ManagerPaths, RedactionManager};
pub use pipeline::{RedactionOutput, RedactionPipeline};
pub use rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};
pub use source::{Caller, RestoreTarget, Source};
pub use toggle::{
    ChannelPolicy, CliFlag, EnvSetting, ForceOverrideFlag, ForceOverrideRecord, ToggleDecision,
    ToggleInputs, ToggleReason, compute_effective_enabled, override_banner,
};
pub use token::Token;
pub use vault::{VaultEntry, VaultStats, VaultStore};
