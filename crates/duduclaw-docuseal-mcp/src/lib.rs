//! DocuSeal MCP server — an MCP stdio wrapper over the DocuSeal REST API.
//!
//! DocuSeal (open-source document signing) ships a built-in MCP endpoint for
//! **self-hosted** instances only (5 tools). This wrapper targets what that
//! leaves uncovered:
//!   * the **cloud** API (`https://api.docuseal.com` / `.eu`, `X-Auth-Token`),
//!   * submission archive, submitter re-send / prefill update,
//!   * signed-document + audit-log URL retrieval.
//!
//! ## Protocol
//!
//! JSON-RPC 2.0, one object per line over stdin/stdout — the same shape the
//! `duduclaw mcp-server` speaks, so any MCP client (including
//! `duduclaw_llm::McpClient`) can mount it. Implements `initialize`,
//! `tools/list`, `tools/call`; everything else answers method-not-found.
//!
//! ## Configuration (environment)
//!
//! | Var | Meaning |
//! |-----|---------|
//! | `DOCUSEAL_API_KEY`  | required — console.docuseal.com/api or instance API settings |
//! | `DOCUSEAL_BASE_URL` | optional — default `https://api.docuseal.com`; EU cloud `https://api.docuseal.eu`; self-hosted `https://<host>/api` |
//!
//! Webhooks (form.completed → autopilot) are configured in the DocuSeal UI —
//! the API has no webhook CRUD; point them at a DuDuClaw channel webhook or
//! the gateway's autopilot event ingress.

pub mod api;
pub mod rpc;
pub mod tools;

pub use api::DocusealApi;
pub use rpc::serve_stdio;
