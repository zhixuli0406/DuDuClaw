//! EvolutionEvents audit-log infrastructure — Sprint N P0.
//!
//! Provides:
//! - [`schema`] — Agnes-confirmed 8-field event schema with 5 event types.
//! - [`logger`] — Non-blocking JSONL appender with day + 10 MB rotation.
//!
//! ## Quick start
//! ```rust,ignore
//! use duduclaw_gateway::evolution_events::{
//!     logger::EvolutionEventLogger,
//!     schema::{AuditEvent, AuditEventType, Outcome},
//! };
//!
//! let logger = EvolutionEventLogger::from_env();
//! let event = AuditEvent::now(AuditEventType::SkillActivate, "my-agent", Outcome::Success)
//!     .with_skill_id("python-patterns")
//!     .with_trigger_signal("skill_auto_activate");
//! logger.log(event).await;
//! ```
//!
//! ## Reserved / future schema fields
//! - **P2** — `intent_category: repair | optimize | innovate`
//!   Do NOT add this field until the P2 sprint is approved.
//!   Documenting here prevents accidental early merges.

pub mod emitter;
pub mod logger;
pub mod query;
pub mod reliability;
pub mod schema;
