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

/// Edit an agent's `agent.toml` and hot-reload the registry.
///
/// The single implementation behind both the dashboard's `agents.update` RPC
/// and the in-chat `/model` command. Writes atomically (temp + rename) and then
/// re-scans the registry — the rescan is deliberately RELIABLE rather than
/// best-effort, because the gateway has no periodic rescan and a skipped one
/// leaves every consumer answering with the old config until a restart.
pub async fn update_agent_toml_with<F>(
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    mutate: F,
) -> Result<bool, String>
where
    F: FnOnce(&mut toml::Table) -> Result<(), String>,
{
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err(format!("Invalid agent_id: {agent_id}"));
    }

    let reg = registry.read().await;
    let agent = reg
        .get(agent_id)
        .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
    let agent_toml_path = agent.dir.join("agent.toml");
    // `agent.dir` is `<home_dir>/agents/<agent_id>` (see `AgentRegistry::new`
    // callers) — walk up two levels rather than threading a separate
    // `home_dir` parameter through every caller of this already-widely-used
    // function just for the model-switch FYI below.
    let home_dir = agent
        .dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    drop(reg);

    let content = tokio::fs::read_to_string(&agent_toml_path)
        .await
        .map_err(|e| format!("Failed to read agent.toml: {e}"))?;

    let mut table: toml::Table = content
        .parse()
        .map_err(|e| format!("Failed to parse agent.toml: {e}"))?;

    let model_before = read_model_preferred(&table);
    mutate(&mut table)?;
    let model_changed = read_model_preferred(&table)
        .is_some_and(|after| Some(after.as_str()) != model_before.as_deref());

    let new_content = toml::to_string_pretty(&table)
        .map_err(|e| format!("Failed to serialise agent.toml: {e}"))?;

    // Atomic write: temp file + rename
    let tmp_path = agent_toml_path.with_extension("toml.tmp");
    tokio::fs::write(&tmp_path, &new_content)
        .await
        .map_err(|e| format!("Failed to write agent.toml.tmp: {e}"))?;
    tokio::fs::rename(&tmp_path, &agent_toml_path)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to commit agent.toml: {e}")
        })?;

    // Registry re-scan for hot-reload. This must be RELIABLE, not
    // best-effort: the gateway has no periodic rescan, so a skipped scan
    // here leaves the in-memory registry stale forever (agents answer —
    // and WebChat displays — the OLD model until the next unrelated
    // update/create or a restart; distributor-reported bug). The write
    // lock is acquired unconditionally — reader guards are all bounded
    // (longest: one system-prompt build), so this waits, it cannot hang.
    // Scan failures (transient IO) retry inline before giving up.
    let mut hot_reloaded = false;
    for attempt in 1..=3u32 {
        let mut reg = registry.write().await;
        match reg.scan().await {
            Ok(()) => {
                hot_reloaded = true;
                break;
            }
            Err(e) => {
                warn!(agent_id, attempt, error = %e, "registry rescan failed after agent.toml write — retrying");
                drop(reg);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    if hot_reloaded {
        // Nudge live WebChat sockets to re-send their session_info frame
        // so open dashboard tabs reflect the change without a reconnect.
        let _ = agent_config_events().send(agent_id.to_string());
        // The channel-side FYI: only when `[model].preferred` actually
        // changed value (not merely re-written), and only once the change is
        // truly live (hot_reloaded) — a rescan failure below leaves the old
        // model answering, so announcing a switch that hasn't landed yet
        // would be a lie.
        if model_changed {
            if let Some(home_dir) = &home_dir {
                crate::pending_agent_notice::mark_model_changed(home_dir, agent_id);
            }
        }
    } else {
        warn!(
            agent_id,
            "registry rescan failed 3× — change persisted to agent.toml but in-memory consumers are stale until the next successful scan"
        );
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
mod progress;
mod prompt_build;

pub(crate) use access_gate::*;
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
