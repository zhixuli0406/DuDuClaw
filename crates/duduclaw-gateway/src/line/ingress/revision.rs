//! The route and authority snapshot of one LINE event.
//!
//! Only what decides who handles this event and with which rights is hashed
//! (review I-HIGH-4 (1)): the resolved employee and its binding, that
//! employee's effective configuration (preset-resolved, I-MEDIUM-4), the
//! LINE credentials digest, and this conversation's channel settings. An
//! unrelated employee being added, edited or broken changes nothing here.
//!
//! "Could not read" ([`RevisionError::Unavailable`]) is kept apart from "read
//! and different" ([`RevisionError::Changed`], or a digest mismatch at the
//! caller): the first backs off, the second quarantines (I-HIGH-3).

use super::super::*;
use duduclaw_agent::resolver::AgentResolver;
use duduclaw_core::types::{Message, MessageType};

/// Why a snapshot could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RevisionError {
    /// Something could not be read or parsed right now (may recover).
    Unavailable(&'static str),
    /// It was read and the event can no longer go where it was accepted.
    Changed(&'static str),
}

/// A computed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Revision {
    pub route: String,
    pub authority: String,
    /// The resolved employee's name (routing key).
    pub agent: String,
}

/// Channel-setting keys that change who may talk to the bot or who answers.
const AUTHORITY_SETTING_KEYS: &[&str] = &[
    keys::ALLOWED_CHANNELS,
    keys::ALLOWED_USERS,
    keys::BLOCKED_USERS,
    keys::REQUIRE_PAIRING,
    keys::ADMIN_USERS,
    keys::MENTION_ONLY,
    keys::AGENT_OVERRIDE,
    keys::SHARED_BOT_BINDING,
];

/// The LINE credentials digest from a parsed `config.toml`. A missing token
/// key is a definite change (LINE was removed); a key that is present but
/// cannot be decrypted is a read failure.
pub(crate) async fn credential_revision(
    home: &Path,
    config: &toml::Table,
) -> Result<String, RevisionError> {
    let channels = config.get("channels").and_then(|v| v.as_table());
    let has_token = channels.is_some_and(|c| {
        c.contains_key("line_channel_token") || c.contains_key("line_channel_token_enc")
    });
    if !has_token {
        return Err(RevisionError::Changed("credentials_removed"));
    }
    let (token, secret) = line_credentials_for_table(home, config)
        .await
        .ok_or(RevisionError::Unavailable("credentials_unreadable"))?;
    if token.is_empty() || secret.is_empty() {
        return Err(RevisionError::Changed("credentials_removed"));
    }
    Ok(crate::channel_ingress::digest(&[&token, &secret]))
}

/// The resolved employee's effective `agent.toml` text: the preset-resolved
/// copy under `<home>/agent_resolved/` when it exists, else `agent.toml`
/// (the selection `duduclaw_core::agent_toml::load` makes), with read errors
/// reported instead of defaulted.
async fn effective_agent_text(agent_dir: &Path) -> Result<String, RevisionError> {
    if let (Some(home), Some(id)) = (
        duduclaw_core::preset::agent_home_dir(agent_dir),
        agent_dir.file_name().and_then(|n| n.to_str()),
    ) {
        let resolved = duduclaw_core::preset::agent_resolved_path(&home, id);
        match tokio::fs::read_to_string(&resolved).await {
            Ok(text) => return Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RevisionError::Unavailable("agent_config_unreadable")),
        }
    }
    // Review L3: a removed employee is a definite change, not a read failure
    // that would back off and retry for nothing.
    tokio::fs::read_to_string(agent_dir.join("agent.toml"))
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => RevisionError::Changed("agent_removed"),
            _ => RevisionError::Unavailable("agent_config_unreadable"),
        })
}

/// Compute the snapshot. `expected_credentials` binds a freshly accepted
/// event to the credentials its signature was checked with.
pub(crate) async fn line_revision(
    state: &LineState,
    event: &LineEvent,
    expected_credentials: Option<&str>,
) -> Result<Revision, RevisionError> {
    let text = tokio::fs::read_to_string(state.home_dir.join("config.toml"))
        .await
        .map_err(|_| RevisionError::Unavailable("config_unreadable"))?;
    let config: toml::Table = text
        .parse()
        .map_err(|_| RevisionError::Unavailable("config_invalid"))?;
    let credentials = credential_revision(&state.home_dir, &config).await?;
    if expected_credentials.is_some_and(|expected| expected != credentials) {
        return Err(RevisionError::Changed("credentials_changed"));
    }
    let sender = event
        .source
        .as_ref()
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("");
    let conversation = line_conversation(event);
    let session = format!("line:{conversation}");
    let message = Message {
        id: String::new(),
        message_type: MessageType::Incoming,
        channel: "line".into(),
        chat_id: session.clone(),
        sender: sender.into(),
        text: event
            .message
            .as_ref()
            .and_then(|m| m.text.as_deref())
            .unwrap_or("")
            .into(),
        timestamp: chrono::Utc::now(),
        agent_id: None,
    };
    let (agent_name, agent_dir) = {
        let reg = state.ctx.registry.read().await;
        let agent = AgentResolver::new(&reg)
            .resolve(&message)
            .ok_or(RevisionError::Changed("no_resolved_agent"))?;
        (agent.config.agent.name.clone(), agent.dir.clone())
    };
    let raw = effective_agent_text(&agent_dir).await?;
    let _: duduclaw_core::types::AgentConfig =
        toml::from_str(&raw).map_err(|_| RevisionError::Unavailable("agent_config_invalid"))?;
    let table: toml::Value = raw
        .parse()
        .map_err(|_| RevisionError::Unavailable("agent_config_invalid"))?;
    let section = |name: &str| table.get(name).cloned();
    let agent_field = |key: &str| table.get("agent").and_then(|t| t.get(key)).cloned();
    let settings = state
        .ctx
        .channel_settings
        .refresh_channel_snapshot_for("line", &conversation, AUTHORITY_SETTING_KEYS)
        .await
        .map_err(|_| RevisionError::Unavailable("channel_settings_unavailable"))?;
    // Invalid persisted authority is not an empty allowlist.
    for file in ["access_control.json", "agent_bindings.json"] {
        match tokio::fs::read(state.home_dir.join(file)).await {
            Ok(bytes) => {
                let _: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| RevisionError::Unavailable("channel_authority_invalid"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RevisionError::Unavailable("channel_authority_unreadable")),
        }
    }
    let binding = state
        .ctx
        .agent_binding
        .resolve_bound_agent("line", sender)
        .await;
    let approved = state.ctx.access_control.runtime_approved_users().await;
    let route = serde_json::json!({
        "resolved": agent_name,
        "trigger": agent_field("trigger"),
        "role": agent_field("role"),
        "status": agent_field("status"),
        "channels": section("channels"),
        "allowed_channels": table.get("permissions").and_then(|v| v.get("allowed_channels")),
        "default_agent": config.get("general").and_then(|v| v.get("default_agent")),
        "binding": binding,
    });
    let identity = [
        "name",
        "role",
        "status",
        "trigger",
        "reports_to",
        "department",
    ]
    .iter()
    .map(|k| (k.to_string(), agent_field(k)))
    .collect::<std::collections::BTreeMap<_, _>>();
    let authority = serde_json::json!({
        "credential_revision": credentials,
        "identity": identity,
        "capabilities": section("capabilities"),
        "permissions": section("permissions"),
        "budget": section("budget"),
        "sandbox_enabled": table.get("container").and_then(|v| v.get("sandbox_enabled")),
        "network_access": table.get("container").and_then(|v| v.get("network_access")),
        "settings": settings,
        "sender_approved": approved.iter().any(|u| u == sender || u == &session),
    });
    Ok(Revision {
        route: crate::channel_ingress::digest(&[&route.to_string()]),
        authority: crate::channel_ingress::digest(&[&authority.to_string()]),
        agent: agent_name,
    })
}
