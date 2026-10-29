//! The gateway owns the turn ledger; all MCP transports use this client.
pub(crate) use duduclaw_gateway::egress_client::{mark, reserve, reservation_failure};
