//! ACP (Agent Communication Protocol) module.
//!
//! Provides reverse-RPC types for file I/O and permission requests,
//! plus an ACP server that generates `.well-known` discovery cards.

pub mod handlers;
pub mod server;
pub mod types;

#[cfg(test)]
mod tests;
