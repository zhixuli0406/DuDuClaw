//! ACP module — two protocols that share an acronym by historical accident:
//!
//! - [`server`] / [`handlers`] / [`message_send`]: the **A2A** protocol server
//!   (`duduclaw acp-server`) — agent-to-agent interop (`agent/discover`,
//!   `message/send`, `tasks/*`) plus `.well-known` discovery cards.
//! - [`client_protocol`]: the **Agent Client Protocol** v1 server
//!   (`duduclaw acp`) — the editor-facing protocol Zed / JetBrains / nvim
//!   agent panels speak (`initialize`, `session/new`, `session/prompt`).

pub mod client_protocol;
pub mod handlers;
pub mod message_send;
pub mod server;
pub mod types;

#[cfg(test)]
mod tests;
