//! v1.68.0 — value types of the `config.toml` keys the gateway and CLI read,
//! for the raw editor's typed validation (`config.raw.set`).
//!
//! Most `config.toml` sections are read raw (`table.get(..).as_bool()`), and
//! those readers silently treat a wrong-typed value as "absent" — so
//! `port = "eighteen"` would be accepted, saved and then quietly ignored. The
//! raw editor therefore checks every key listed here against its expected
//! TOML type and refuses a mismatch with the line/column of the value.
//! Unknown sections and unknown keys stay allowed (forward compatibility,
//! operator notes); sections with a strict typed struct (`[container.sandbox]`,
//! `[computer_use]`, `[[tick.sources]]`, `[team]`, `[takeover]`,
//! `[redaction]`, `[db_sources]`, `[secret_manager]`) are additionally
//! deserialized by `config_raw_rpc::validate_config_table`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Ty {
    Bool,
    Str,
    /// Non-negative integer.
    UInt,
    /// Port number 1-65535.
    Port,
    /// Float (an integer literal is accepted too).
    Float,
    StrArray,
    Table,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::Bool => "true or false",
            Ty::Str => "a string",
            Ty::UInt => "a non-negative integer",
            Ty::Port => "a port number (1-65535)",
            Ty::Float => "a number",
            Ty::StrArray => "an array of strings",
            Ty::Table => "a table",
        }
    }

    fn accepts(self, v: &toml::Value) -> bool {
        match self {
            Ty::Bool => v.is_bool(),
            Ty::Str => v.is_str(),
            Ty::UInt => v.as_integer().is_some_and(|n| n >= 0),
            Ty::Port => v.as_integer().is_some_and(|n| (1..=65_535).contains(&n)),
            Ty::Float => v.is_float() || v.is_integer(),
            Ty::StrArray => v.as_array().is_some_and(|a| a.iter().all(|x| x.is_str())),
            Ty::Table => v.is_table(),
        }
    }
}

use Ty::*;

/// `(section path, key, type)` for every typed key a reader consumes.
pub(crate) const CONFIG_KEY_TYPES: &[(&str, &str, Ty)] = &[
    ("general", "log_level", Str),
    ("general", "default_agent", Str),
    ("general", "default_language", Str),
    ("general", "inference_mode", Str),
    ("general", "name", Str),
    ("gateway", "bind", Str),
    ("gateway", "port", Port),
    ("gateway", "auth_token", Str),
    ("gateway", "auth_token_enc", Str),
    ("gateway", "allowed_origins", StrArray),
    ("gateway", "auto_update", Bool),
    ("gateway", "local_auto_login", Bool),
    ("dashboard", "local_auto_login", Bool),
    ("dashboard", "public_url", Str),
    ("server", "mdns_advertise", Bool),
    ("server", "tls", Bool),
    ("rotation", "strategy", Str),
    ("rotation", "cooldown_after_rate_limit_seconds", UInt),
    ("rotation", "health_check_interval_seconds", UInt),
    ("memory", "novelty_gate", Bool),
    ("memory", "graph_embed_seed", Bool),
    ("memory", "graph_embed_seed_top_k", UInt),
    ("memory", "supersession_trust_guard", Bool),
    ("memory", "recent_actions_enabled", Bool),
    ("memory", "recent_actions_count", UInt),
    ("memory", "working_state_enabled", Bool),
    ("memory", "working_state_handoff_max_bytes", UInt),
    ("notify", "daily_digest", Bool),
    ("notify", "daily_digest_at", Str),
    ("notify", "quiet_hours", Str),
    ("skills", "gap_digest_enabled", Bool),
    ("miniapp", "enabled", Bool),
    ("knowledge_guard", "enabled", Bool),
    ("knowledge_guard", "window_secs", UInt),
    ("knowledge_guard", "max_per_subject", UInt),
    ("topology_evolution", "enabled", Bool),
    ("topology_evolution", "lookback_days", UInt),
    ("topology_evolution", "min_samples", UInt),
    ("topology_evolution", "reject_rate_threshold", Float),
    ("topology_evolution", "observe_hours", UInt),
    ("topology_evolution", "proposal_cooldown_days", UInt),
    ("topology_evolution", "tick_secs", UInt),
    ("topology_evolution", "approval_ttl_secs", UInt),
    ("belief", "flat_band_pct", Float),
    ("belief", "tick_subject_map", Table),
    ("goal_loop", "planner_enabled", Bool),
    ("goal_loop", "iteration_cap_simple", UInt),
    ("goal_loop", "iteration_cap", UInt),
    ("goal_loop", "resume_on_restart", Str),
    ("goal_loop", "soft_cap", UInt),
    ("goal_loop", "wall_clock_hours", UInt),
    ("goal_loop", "max_concurrent", UInt),
    ("goal_loop", "tick_secs", UInt),
    ("goal_loop", "stalled_secs", UInt),
    ("goal_loop", "progress_report_minutes", UInt),
    ("goal_loop", "tool_streak_advisory", Bool),
    ("dispatch", "enabled", Bool),
    ("dispatch", "policy", Str),
    ("dispatch", "judge", Str),
    ("dispatch", "judge_provider", Str),
    ("dispatch", "judge_model", Str),
    ("dispatch", "judge_command", StrArray),
    ("dispatch", "judge_timeout_secs", UInt),
    ("dispatch", "two_stage_judge", Bool),
    ("dispatch", "grounding_precheck_enabled", Bool),
    ("dispatch", "grounding_min_overlap_chars", UInt),
    ("dispatch", "admission", Str),
    ("dispatch", "queue_max_depth", UInt),
    ("dispatch", "queue_item_ttl_secs", UInt),
    ("dispatch", "ephemeral_max_active", UInt),
    ("dispatch", "personal_max_concurrent", UInt),
    ("dispatch", "concurrency_lease_ttl_secs", UInt),
    ("dispatch.team_budget", "max_spawns_per_task", UInt),
    ("dispatch.team_budget", "max_turns_per_role", UInt),
    ("dispatch.team_budget", "degrade_order", StrArray),
    ("dispatch_guard", "window_secs", UInt),
    ("dispatch_guard", "max_in_window", UInt),
    ("dispatch_guard", "role_team_max_in_window", UInt),
    ("dispatch_guard", "cooldown_secs", UInt),
    ("dispatch_guard", "max_hop_depth", UInt),
    ("backup", "schedule_enabled", Bool),
    ("backup", "interval_hours", UInt),
    ("backup", "retention_count", UInt),
    ("skill_synthesis", "auto_run", Bool),
    ("skill_synthesis", "dry_run", Bool),
    ("skill_synthesis", "interval_hours", UInt),
    ("skill_synthesis", "lookback_days", UInt),
    ("skill_synthesis", "target_agent", Str),
    ("task_forward_model", "enabled", Bool),
    ("task_forward_model", "calibration_enabled", Bool),
    ("task_forward_model", "held_out_gate_enabled", Bool),
    ("task_forward_model", "rule_induction", Bool),
    ("delegation", "policy", Str),
    ("delegation", "require_identity_token", Bool),
    ("delegation", "confidence_routing", Bool),
    ("mail", "enabled", Bool),
    ("mail", "gmail_enabled", Bool),
    ("mail", "gmail_query", Str),
    ("mail", "dropfolder_enabled", Bool),
    ("mail", "poll_interval_secs", UInt),
    ("mail", "default_agent", Str),
    ("mail", "auto_trigger", Bool),
    ("mail", "allowed_senders", StrArray),
    ("mail", "allowed_recipients", StrArray),
    ("mail", "max_body_chars", UInt),
    ("mail", "outbound_ttl_secs", UInt),
    ("webchat", "public_widget", Bool),
    ("webchat", "widget_key", Str),
    ("files", "allowed_roots", StrArray),
    ("night", "llm_enabled", Bool),
    ("tick", "enabled", Bool),
    ("tick", "allow_command_sources", Bool),
    ("tick", "dns_ttl_secs", UInt),
    ("tick", "preset", Str),
    ("acp", "trusted", Bool),
    ("telemetry", "otlp_endpoint", Str),
    ("telemetry", "otlp_protocol", Str),
    ("telemetry", "service_name", Str),
    ("telemetry", "sample_ratio", Float),
    ("telemetry", "otlp_headers", Table),
    ("logging", "format", Str),
    ("integrations", "google_workspace", Bool),
    ("integrations", "github", Bool),
    ("ccr", "enabled", Bool),
    ("ccr", "ttl_seconds", UInt),
    ("ccr", "max_entries", UInt),
    ("ccr", "min_compress_bytes", UInt),
    ("ccr", "builtin_sources", Bool),
    ("decision", "enabled", Bool),
    ("decision", "task_board_shadow", Bool),
    ("decision", "task_board_shadow_every_hours", UInt),
    ("decision", "task_board_retention_days", UInt),
    ("decision", "task_board_horizon_days", UInt),
    ("decision", "task_board_queue", Str),
    ("office", "delivery_gate", Bool),
    ("office", "delivery_gate_placeholder_block", Bool),
    ("api", "anthropic_api_key", Str),
    ("api", "anthropic_api_key_enc", Str),
    ("runtime", "utility_provider", Str),
    ("runtime", "utility_model", Str),
    ("proactive", "natural_timing", Bool),
    ("branding", "reply_footer", Bool),
    ("evolution", "fault_attribution", Bool),
    ("evolution", "eval_suites_root", Str),
    ("evolution", "eval_binary", Str),
    ("evolution", "require_causal_evidence", Bool),
    ("voice", "stt_provider", Str),
    ("voice", "stt_base_url", Str),
    ("voice", "stt_model", Str),
    ("voice", "stt_command", Str),
    ("voice", "stt_api_key", Str),
    ("voice", "stt_api_key_enc", Str),
    ("causal", "audit_ingest", Bool),
    ("account_loading", "inherit_host_credentials", Bool),
    ("goal_defaults", "baseline_boundary", Str),
    ("task_board", "review_wip_limit", UInt),
];

/// Check every listed key that is present. `text` is the file the table was
/// parsed from, used for the line/column of the offending value.
pub(crate) fn check_config_types(text: &str, table: &toml::Table) -> Result<(), String> {
    for (section, key, ty) in CONFIG_KEY_TYPES {
        let mut cur = Some(table);
        for seg in section.split('.') {
            cur = cur.and_then(|t| t.get(seg)).and_then(|v| v.as_table());
        }
        let Some(value) = cur.and_then(|t| t.get(*key)) else { continue };
        if !ty.accepts(value) {
            return Err(format!(
                "[{section}] {key} must be {}{}",
                ty.name(),
                location(text, section, key)
            ));
        }
    }
    Ok(())
}

/// ` (line L, column C)` of `[section] key`'s value, from an
/// immutable `toml_edit` parse (which keeps spans); "" when not found.
fn location(text: &str, section: &str, key: &str) -> String {
    let Ok(doc) = toml_edit::ImDocument::parse(text) else { return String::new() };
    let mut cur: Option<&dyn toml_edit::TableLike> = Some(doc.as_table());
    for seg in section.split('.') {
        cur = cur.and_then(|t| t.get(seg)).and_then(|i| i.as_table_like());
    }
    let span = cur.and_then(|t| t.get(key)).and_then(|i| i.span());
    match span {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            format!(
                " (line {}, column {})",
                before.matches('\n').count() + 1,
                before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1
            )
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Result<(), String> {
        check_config_types(text, &text.parse().unwrap())
    }

    #[test]
    fn wrong_types_are_refused_with_a_location() {
        let e = check("[gateway]\nbind = \"127.0.0.1\"\nport = \"eighteen\"\n").unwrap_err();
        assert!(e.contains("[gateway] port") && e.contains("line 3"), "{e}");
        assert!(check("[gateway]\nlocal_auto_login = \"no\"\n").is_err());
        assert!(check("[night]\nllm_enabled = \"nah\"\n").is_err());
        assert!(check("[gateway]\nport = 70000\n").is_err());
        assert!(check("[dispatch.team_budget]\nmax_spawns_per_task = -1\n").is_err());
    }

    #[test]
    fn right_types_and_unknown_keys_pass() {
        check("[gateway]\nport = 18789\nfuture_key = \"x\"\n[belief]\nflat_band_pct = 1\n[mystery]\nx = 1\n").unwrap();
    }
}
