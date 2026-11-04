use duduclaw_core::types::Message;

use crate::registry::{AgentRegistry, LoadedAgent};

/// Routes incoming messages to the appropriate agent based on channel
/// bindings and fallback rules.
///
/// Calling an employee by `@name` is NOT handled here: who may call whom
/// depends on the sender, so trigger routing lives in the gateway
/// (`trigger_route`, built on [`crate::trigger_match`]) and runs before this
/// resolver. This type only answers "who handles a message that names
/// nobody".
pub struct AgentResolver<'a> {
    registry: &'a AgentRegistry,
}

impl<'a> AgentResolver<'a> {
    /// Create a new resolver backed by the given registry.
    pub fn new(registry: &'a AgentRegistry) -> Self {
        Self { registry }
    }

    /// Resolve which agent handles a message that calls no employee.
    ///
    /// Resolution order:
    /// 1. **Channel/Thread binding** (RFC-22 Decision 3-D, Phase 3 W3) —
    ///    `[[channels.discord.bindings]]` entries in `agent.toml`. Resolves
    ///    `discord:thread:<id>` and `discord:<channel_id>` directly to the
    ///    bound agent so sub-agents receive channel messages without
    ///    going through the root agent first (which previously caused
    ///    14-day SOUL stagnation for 16 of 17 sub-agents).
    /// 2. Coarse permission grant — the message channel name (e.g. "discord")
    ///    is in the agent's `permissions.allowed_channels` list.
    /// 3. Fall back to the main agent (role = Main).
    ///
    /// Before 1.76 a step 0 matched `[agent] trigger` with an unanchored,
    /// sender-blind `contains`; that let anyone reach any employee.
    pub fn resolve(&self, message: &Message) -> Option<&'a LoadedAgent> {
        let agents = self.registry.list();
        let configs: Vec<_> = agents.iter().map(|agent| &agent.config).collect();
        // Conversation routing historically matches bindings by session shape,
        // including WebChat threads. History authorization stays channel-bound.
        resolve_config(&configs, message, true)
            .and_then(|selected| agents.iter().copied().find(|agent| agent.config.agent.name == selected.agent.name))
            .or_else(|| self.registry.main_agent())
    }

    /// Resolve only administrator-configured bindings or channel permissions.
    /// History reads must never acquire access through the main-agent fallback.
    pub fn resolve_explicit(&self, message: &Message) -> Option<&'a LoadedAgent> {
        let agents = self.registry.list();
        let configs: Vec<_> = agents.iter().map(|agent| &agent.config).collect();
        let selected = resolve_explicit_config(&configs, message)?;
        agents.into_iter().find(|agent| agent.config.agent.name == selected.agent.name)
    }
}

/// The same explicit route over a read-only configuration snapshot. MCP reads
/// do not load skills or materialize preset artifacts merely to authorize data.
pub fn resolve_explicit_config<'a>(
    configs: &[&'a duduclaw_core::types::AgentConfig], message: &Message,
) -> Option<&'a duduclaw_core::types::AgentConfig> {
    resolve_config(configs, message, false)
}

fn resolve_config<'a>(
    configs: &[&'a duduclaw_core::types::AgentConfig], message: &Message,
    conversation_routing: bool,
) -> Option<&'a duduclaw_core::types::AgentConfig> {
    let mut configs = configs.to_vec();
    configs.sort_by(|a, b| a.agent.name.cmp(&b.agent.name));
    if conversation_routing || message.channel == "discord" {
        if let Some((kind, id)) = parse_session_binding(&message.chat_id) {
            for config in &configs {
                if let Some(discord) = config.channels.as_ref().and_then(|channels| channels.discord.as_ref()) {
                    if discord.bindings.iter().any(|binding| binding_matches(&binding.kind, &binding.id, kind, id)) {
                        return Some(*config);
                    }
                }
            }
        }
    }
    configs.into_iter().find(|config| config.permissions.allowed_channels.iter().any(|channel| channel == &message.channel))
}

/// Parse a session id string into a `(kind, id)` pair for binding lookup.
///
/// Supported shapes:
/// - `discord:thread:<id>` → `("thread", "<id>")`
/// - `discord:<channel_id>` → `("channel", "<channel_id>")`
/// - `telegram:<chat_id>` / `line:<id>` → `("channel", "<id>")` (currently
///   discord-only at the resolver layer; included for forward compatibility).
///
/// Returns `None` for malformed inputs (no `:` at all).
fn parse_session_binding(session: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = session.splitn(3, ':').collect();
    match parts.len() {
        3 if parts[1] == "thread" => Some(("thread", parts[2])),
        3 => Some(("channel", parts[1])),
        2 => Some(("channel", parts[1])),
        _ => None,
    }
}

/// True when a configured binding (`cfg_kind`, `cfg_id`) matches the
/// session-derived (`msg_kind`, `msg_id`).  Unknown `cfg_kind` values are
/// treated as no-match (fail-closed).
fn binding_matches(cfg_kind: &str, cfg_id: &str, msg_kind: &str, msg_id: &str) -> bool {
    match cfg_kind {
        "thread" | "channel" => cfg_kind == msg_kind && cfg_id == msg_id,
        "guild" => false, // reserved for future expansion
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_thread_session() {
        assert_eq!(
            parse_session_binding("discord:thread:1501225251910979704"),
            Some(("thread", "1501225251910979704"))
        );
    }

    #[test]
    fn parse_channel_session() {
        assert_eq!(
            parse_session_binding("discord:1495730722156318901"),
            Some(("channel", "1495730722156318901"))
        );
    }

    #[test]
    fn parse_telegram_session_treats_as_channel() {
        // Forward-compat: when telegram bindings land, this should match.
        assert_eq!(
            parse_session_binding("telegram:12345"),
            Some(("channel", "12345"))
        );
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert_eq!(parse_session_binding(""), None);
        assert_eq!(parse_session_binding("nocolon"), None);
    }

    #[test]
    fn binding_matches_thread_exact() {
        assert!(binding_matches("thread", "abc", "thread", "abc"));
        assert!(!binding_matches("thread", "abc", "thread", "xyz"));
        assert!(!binding_matches("thread", "abc", "channel", "abc"));
    }

    #[test]
    fn binding_matches_channel_exact() {
        assert!(binding_matches("channel", "abc", "channel", "abc"));
        assert!(!binding_matches("channel", "abc", "thread", "abc"));
    }

    #[test]
    fn binding_matches_guild_reserved_no_match() {
        // guild bindings are reserved; do not yet match anything.
        assert!(!binding_matches("guild", "abc", "channel", "abc"));
        assert!(!binding_matches("guild", "abc", "thread", "abc"));
    }

    #[test]
    fn binding_matches_unknown_kind_fail_closed() {
        assert!(!binding_matches("nonsense", "abc", "channel", "abc"));
        assert!(!binding_matches("", "abc", "channel", "abc"));
    }
}

#[cfg(test)]
mod resolve_order_tests {
    use super::*;
    use chrono::Utc;
    use duduclaw_core::types::{Message, MessageType};
    use tempfile::TempDir;

    /// Write an agent dir whose agent.toml grants `discord` channel access,
    /// so every agent matches the coarse-permission rule (step 3) — the
    /// branch where ordering across multiple matches actually matters.
    ///
    /// The config is a minimal but currently-valid `agent.toml` (mirrors the
    /// `duduclaw init` scaffold) so the test does not depend on external
    /// template files that may drift out of sync with the schema.
    fn write_agent(root: &std::path::Path, name: &str) {
        let toml = format!(
            r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🐾"

[model]
preferred = "claude-haiku-4-5"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
api_mode = "cli"

[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["discord"]

[evolution]
skill_auto_activate = false
skill_security_scan = true
gvu_enabled = false
cognitive_memory = false
max_silence_hours = 168.0
max_gvu_generations = 0
observation_period_hours = 24.0
skill_token_budget = 500
max_active_skills = 2
"#
        );
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), toml).unwrap();
    }

    fn discord_message() -> Message {
        Message {
            id: "m1".to_string(),
            message_type: MessageType::Incoming,
            channel: "discord".to_string(),
            chat_id: "discord:999".to_string(),
            sender: "u".to_string(),
            text: "hello".to_string(),
            timestamp: Utc::now(),
            agent_id: None,
        }
    }

    /// M40 regression: when several agents match the same message, resolution
    /// must be deterministic (same agent every time) instead of depending on
    /// HashMap iteration order. The resolver sorts by name, so the
    /// alphabetically-first matching agent always wins.
    #[tokio::test]
    async fn resolve_is_deterministic_across_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        // Names deliberately out of alphabetical creation order.
        for name in ["zeta", "alpha", "mike", "beta"] {
            write_agent(tmp.path(), name);
        }

        let mut registry = AgentRegistry::new(tmp.path().to_path_buf());
        registry.scan().await.unwrap();
        assert_eq!(registry.list().len(), 4, "all four agents should load");

        let resolver = AgentResolver::new(&registry);
        let msg = discord_message();

        // Resolve many times — every call must return the same agent, and it
        // must be the alphabetically-first ("alpha"), independent of HashMap
        // ordering between runs.
        let first = resolver
            .resolve(&msg)
            .expect("a discord-permitted agent should resolve")
            .config
            .agent
            .name
            .clone();
        assert_eq!(first, "alpha", "lowest-sorted matching agent should win");
        for _ in 0..20 {
            let again = resolver.resolve(&msg).expect("should resolve").config.agent.name.clone();
            assert_eq!(again, first, "resolution must be stable across calls");
        }
    }
}
