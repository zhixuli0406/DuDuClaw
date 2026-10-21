//! Agent-definition importer — one front door (`duduclaw pack install`,
//! dashboard「匯入 AI 團隊」) for the agent formats in common use, so an
//! employee defined elsewhere arrives with its persona, operating
//! instructions, user profile, memory and skills in the right DuDuClaw
//! slots instead of being rejected.
//!
//! ```text
//! source ─ safe_zip (junk filter, filename decoding) ─ detect ─ adapter ─▶ ImportBundle
//!                                                                     │
//!   writer: ids/conflicts → archive original → scaffold (SOUL) → slots ┤
//!           (IDENTITY / INSTRUCTIONS / wiki / memory) → tools (narrow)  │
//!           → MCP (no secrets) → skills (enable | quarantine | block)  ▼
//!                                                               install record + report
//! ```
//!
//! Formats: 豆包夥伴安裝包, OpenClaw (state dir or one workspace), Hermes,
//! AGENTS.md / CLAUDE.md / GEMINI.md, Claude Code subagents + Copilot
//! `.agent.md`, Dify app DSL. Unknown layouts are still rejected with a
//! listing by the caller. The same slot writer backs `duduclaw migrate
//! from openclaw|hermes` ([`slots`], [`adapters::read_workspace`]).

pub mod adapters;
pub mod detect;
pub mod ir;
pub mod md;
mod names;
pub mod outcome;
mod placement;
pub mod skills;
pub mod slots;
pub mod vendor;
mod writer;

use std::path::Path;

use duduclaw_core::error::Result;

pub use detect::ConfigKind;

use super::install::InstallCtx;
use super::{InstallRecord, Report, cfg_err};

/// Read `root` as `kind` and install it (or plan it, in a dry run).
pub(in crate::expert) async fn install(
    ctx: &InstallCtx,
    root: &Path,
    kind: ConfigKind,
    report: &mut Report,
) -> Result<InstallRecord> {
    let bundle = adapters::read(kind, root).map_err(cfg_err)?;
    writer::install(ctx, bundle, root, report).await
}

#[cfg(test)]
mod tests;
