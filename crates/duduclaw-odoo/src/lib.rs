pub mod agent_config;
pub mod config;
pub mod connector;
pub mod edition;
pub mod events;
pub mod models;
pub mod rpc;
/// X1 方案 4 — Odoo Helpdesk / Project as a decision-twin support queue.
pub mod support_export;

pub use agent_config::{AgentOdooConfig, OdooConfigResolver};
pub use config::OdooConfig;
pub use connector::{
    check_blocklist, is_introspection_noise, AccessKind, BlockDenial, OdooConnector, OdooStatus,
    SchemaField, SchemaModel, SchemaReport, PARTNER_SEARCH_FIELDS,
};
pub use edition::{Edition, EditionGate};
pub use events::{OdooEvent, PollTracker};
