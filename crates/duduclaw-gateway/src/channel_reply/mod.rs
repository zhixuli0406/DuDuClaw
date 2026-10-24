//! Shared AI reply builder for all channel bots.
//!
//! Calls the Claude Code SDK (Python) via subprocess for AI responses,
//! using the multi-account rotator for key management and budget tracking.
//! Falls back to direct Anthropic API if Python is unavailable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_agent::resolver::AgentResolver;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use duduclaw_core::types::{Message, MessageType};
use duduclaw_security::circuit_breaker::CircuitBreakerRegistry;
use duduclaw_security::failsafe::FailsafeManager;
use duduclaw_security::killswitch::KillswitchConfig;

use crate::channel_settings::ChannelSettingsManager;
use crate::evolution_events::emitter::EvolutionEventEmitter;
use crate::gvu::loop_::GvuLoop;
use crate::handlers::ChannelState;
use crate::prediction::engine::PredictionEngine;
use crate::session::SessionManager;
use crate::skill_extraction::recorder::{
    Sentiment, SkillCache, SkillExtractor, TrajectoryOutcome, TrajectoryRecorder,
};
use crate::skill_lifecycle::activation::SkillActivationController;
use crate::skill_lifecycle::compression::CompressedSkillCache;
use crate::skill_lifecycle::gap_accumulator::GapAccumulator;
use crate::skill_lifecycle::lift::LiftTrackerStore;
use crate::skill_lifecycle::sandbox_trial::SandboxStore;

/// Opening literal of the sender-metadata line prepended to every stored user
/// message. Shared by the writer and [`strip_sender_prefix`] so the two can
/// never drift.
pub const SENDER_PREFIX_OPEN: &str = "[sender_id: ";

/// Remove the `[sender_id: …]` metadata line from a stored user message.
///
/// The prefix exists so the model knows who is speaking in a group chat, but it
/// is internal plumbing: when it reaches a human it shows up as
/// `[sender_id: webchat:127.0.0.1:c8c8bb27]` above their own words, and — worse
/// — as the *title* of the conversation in the sidebar, because an untitled
/// session falls back to its first user message. Every display path (transcript
/// replay, session listing) strips it.
///
/// Only a well-formed marker is removed: the line must open with the exact
/// literal, close with `]`, and be a single line. Text a user happened to type
/// that merely resembles it is left alone.
pub fn strip_sender_prefix(text: &str) -> &str {
    let Some(rest) = text.strip_prefix(SENDER_PREFIX_OPEN) else {
        return text;
    };
    // The id itself never contains a newline; refuse to swallow more than one
    // line if the close bracket is missing.
    let Some(close) = rest.find(']') else {
        return text;
    };
    if rest[..close].contains('\n') {
        return text;
    }
    match rest[close + 1..].strip_prefix('\n') {
        Some(body) => body,
        // `[sender_id: x]` with nothing after it — the whole message was the
        // marker; there is no body to show.
        None if rest[close + 1..].is_empty() => "",
        None => text,
    }
}

/// Who is editing `agent.toml` through [`update_agent_toml_edit`].
pub struct AgentTomlEdit<'a, G> {
    /// Recorded on the version and the audit row (`agents.update`,
    /// `chat.model`, `channels.add`, …).
    pub source: &'static str,
    pub user_id: &'a str,
    /// Re-checked inside the file lock, right before the write (a live
    /// permission check for dashboard callers; `|| Ok(())` for system paths).
    pub guard: G,
}

/// Edit an agent's `agent.toml` and hot-reload the registry.
///
/// The single implementation behind the dashboard's `agents.update` RPC, the
/// in-chat `/model` command and every other internal `agent.toml` edit. Runs
/// through the profile file writer (`agent_files::writer`): the mutation
/// runs on the file as read under the cross-process lock (so no concurrent
/// edit is lost), the result is rendered format-preserving (comments and key
/// order survive), a version is kept and the change is audited, then the
/// registry is re-scanned RELIABLY — the gateway has no periodic rescan, so
/// a skipped one leaves every consumer answering with the old config until a
/// restart.
pub async fn update_agent_toml_with<F>(
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    mutate: F,
) -> Result<bool, String>
where
    F: FnOnce(&mut toml::Table) -> Result<(), String>,
{
    let edit = AgentTomlEdit { source: "agents.update", user_id: "system", guard: || Ok(()) };
    update_agent_toml_edit(registry, agent_id, edit, mutate).await
}

/// [`update_agent_toml_with`] naming the source and the caller.
pub async fn update_agent_toml_edit<F, G>(
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    edit: AgentTomlEdit<'_, G>,
    mutate: F,
) -> Result<bool, String>
where
    F: FnOnce(&mut toml::Table) -> Result<(), String>,
    G: FnOnce() -> Result<(), String>,
{
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err(format!("Invalid agent_id: {agent_id}"));
    }
    let home_dir = {
        let reg = registry.read().await;
        let agent = reg
            .get(agent_id)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
        // `agent.dir` is `<home_dir>/agents/<agent_id>`.
        agent
            .dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?
    };

    let model_changed = std::sync::atomic::AtomicBool::new(false);
    let render = |current: &str| -> Result<String, String> {
        let mut table: toml::Table = current
            .parse()
            .map_err(|e| format!("Failed to parse agent.toml: {e}"))?;
        let model_before = read_model_preferred(&table);
        mutate(&mut table)?;
        model_changed.store(
            read_model_preferred(&table).is_some_and(|after| Some(after.as_str()) != model_before.as_deref()),
            std::sync::atomic::Ordering::Relaxed,
        );
        crate::handlers::config_commit::render_preserving(current, &table)
    };
    let outcome = crate::agent_files::writer::commit(
        registry,
        crate::agent_files::writer::Commit {
            home: &home_dir,
            agent_id,
            key: crate::agent_files::FileKey::AgentToml,
            expected_hash: None,
            meta: crate::agent_files::writer::CommitMeta::new(edit.user_id, edit.source),
            baseline: crate::agent_files::writer::Baseline::Accept,
        },
        render,
        edit.guard,
    )
    .await
    .map_err(|e| e.message())?;

    if outcome.unchanged {
        return Ok(true);
    }
    let hot_reloaded = outcome.registry_reloaded;
    if hot_reloaded {
        // Nudge live WebChat sockets to re-send their session_info frame
        // so open dashboard tabs reflect the change without a reconnect.
        let _ = agent_config_events().send(agent_id.to_string());
        // The channel-side FYI: only when `[model].preferred` actually
        // changed value (not merely re-written), and only once the change is
        // truly live — announcing a switch that hasn't landed would be a lie.
        if model_changed.load(std::sync::atomic::Ordering::Relaxed) {
            crate::pending_agent_notice::mark_model_changed(&home_dir, agent_id);
        }
    }
    Ok(hot_reloaded)
}

/// Read `[model].preferred` out of a parsed `agent.toml` table, if present.
/// Used by [`update_agent_toml_with`] to detect an actual model switch
/// (before-vs-after comparison), never to drive dispatch itself.
fn read_model_preferred(table: &toml::Table) -> Option<String> {
    table
        .get("model")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("preferred"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Shared channel status map, accessible by both channel bots and the RPC handler.
pub type ChannelStatusMap = Arc<RwLock<std::collections::HashMap<String, ChannelState>>>;

// ── Multi-turn conversation types ──────────────────────────

/// A single turn in conversation history, used for native multi-turn support.
///
/// Re-exported from [`crate::runtime`] so the channel-reply path and the runtime
/// trait path share one type instead of two structurally-identical copies
/// (RFC-25 A1).
pub use crate::runtime::ConversationTurn;


// ── Submodules (audit O6 file split; pure code motion) ──────
//
// This module was one 11,684-line file. It is now a directory module
// whose submodules hold the same code verbatim; every path that used to
// resolve through `crate::channel_reply::…` still does, via the
// re-exports below.

mod access_gate;
mod breaker_wait;
mod ccr_delivery;
mod cli_env;
mod cli_spawn;
mod context;
mod delivery;
mod direct_api;
mod entry;
mod failure;
mod failure_message;
mod guarded;
mod history;
mod inner;
pub mod interim;
mod progress;
mod prompt_build;

pub(crate) use access_gate::*;
pub(crate) use breaker_wait::as_sender;
pub use ccr_delivery::*;
use cli_env::*;
pub use cli_spawn::*;
pub use context::*;
pub use delivery::*;
pub use direct_api::*;
pub use entry::*;
pub(crate) use failure::*;
pub(crate) use failure_message::*;
pub use guarded::*;
pub(crate) use history::*;
pub(crate) use inner::*;
pub use progress::*;
use prompt_build::*;

#[cfg(test)]
mod tests;

mod reply_origin;
use reply_origin::{ReplyAttempt,ReplyFailure,ReplyOrigin,direct_api_allowed};
#[cfg(test)]
mod origin_tests;
