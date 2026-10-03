//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── System Config Update ─────────────────────────────────

    /// Update system-level config.toml fields (whitelist only).
    ///
    /// Only allows safe, non-sensitive fields: `log_level`, `rotation_strategy`.
    /// Uses atomic write (temp + rename) and never touches token/key fields.
    pub(crate) async fn handle_system_update_config(&self, params: Value, ctx: &UserContext) -> WsFrame {
        // v1.68: the contract names parameters after their TOML path and may
        // send them nested; map those onto the flat names this handler reads.
        let params = super::system_update_config_v168::normalize_legacy_aliases(&params);
        let config_path = self.home_dir.join("config.toml");
        // Read the text (not just the table) so the commit below can refuse
        // to overwrite a file another writer changed in the meantime. A
        // present-but-unparsable file is refused outright: the previous
        // `read_config_table` fallback turned it into an empty table and the
        // save then replaced the whole config with the few keys it carried.
        let original_text = match super::config_commit::read_text_or_empty(&config_path) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let original_hash = super::config_commit::content_hash(&original_text);
        let mut table: toml::Table = match original_text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("config.toml is not valid TOML, refusing to rewrite it (fix it in the raw editor first): {e}"),
                );
            }
        };
        let mut changes: Vec<String> = Vec::new();
        let mut restart_required: Vec<String> = Vec::new();
        // Cleaned remote-access allowlist to hot-apply AFTER a successful write
        // (Some(..) iff the payload carried `allowed_origins`).
        let mut applied_origins: Option<Vec<String>> = None;

        // ── log_level ──
        if let Some(v) = params.get("log_level").and_then(|v| v.as_str()) {
            match v {
                "trace" | "debug" | "info" | "warn" | "error" => {
                    // `[general] log_level` is the key the CLI actually reads
                    // at startup (`read_log_level_from_config`, precedence
                    // RUST_LOG → config → default). Until 2026-09-06 this
                    // wrote `[logging] level`, which nothing reads — the
                    // dashboard reported success and the level never
                    // changed.
                    let general = table
                        .entry("general")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(general) = general {
                        general.insert("log_level".into(), toml::Value::String(v.into()));
                        changes.push(format!("general.log_level = \"{v}\""));
                    }
                }
                _ => {
                    return WsFrame::error_response(
                        "",
                        &format!("Invalid log_level '{v}'. Valid: trace, debug, info, warn, error"),
                    );
                }
            }
        }

        // ── rotation_strategy ──
        if let Some(v) = params.get("rotation_strategy").and_then(|v| v.as_str()) {
            match v {
                "priority" | "round_robin" | "least_cost" | "failover" => {
                    let rotation = table
                        .entry("rotation")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(rotation) = rotation {
                        rotation.insert("strategy".into(), toml::Value::String(v.into()));
                        changes.push(format!("rotation.strategy = \"{v}\""));
                    }
                }
                _ => {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "Invalid rotation_strategy '{v}'. Valid: priority, round_robin, least_cost, failover"
                        ),
                    );
                }
            }
        }

        // ── auto_update (Pro only) ──
        if let Some(v) = params.get("auto_update").and_then(|v| v.as_bool()) {
            let gateway = table
                .entry("gateway")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(gateway) = gateway {
                gateway.insert("auto_update".into(), toml::Value::Boolean(v));
                changes.push(format!("gateway.auto_update = {v}"));
            }
        }

        // ── G.1 [gateway] bind / port / auth_token (restart required) ──
        // bind/port/auth_token change the listening socket + admin token, which
        // are read once at gateway start — we persist + flag, never hot-apply.
        {
            let has_gw = ["bind", "port", "auth_token"]
                .iter()
                .any(|k| params.get(*k).is_some());
            if has_gw {
                let gateway = table
                    .entry("gateway")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                if let Some(v) = params.get("bind").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    // Fail-closed: only a literal IP is accepted (127.0.0.1 /
                    // 0.0.0.0 / custom). Rejects hostnames + injection strings.
                    if !is_valid_bind_addr(v) {
                        return WsFrame::error_response(
                            "",
                            "gateway.bind must be a valid IP address (e.g. 127.0.0.1 or 0.0.0.0)",
                        );
                    }
                    gateway.insert("bind".into(), toml::Value::String(v.into()));
                    changes.push(format!("gateway.bind = \"{v}\" (restart required)"));
                }
                if let Some(v) = params.get("port").and_then(|v| v.as_u64()) {
                    if v == 0 || v > 65535 {
                        return WsFrame::error_response("", "gateway.port must be 1-65535");
                    }
                    gateway.insert("port".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("gateway.port = {v} (restart required)"));
                }
                if let Some(v) = params.get("auth_token").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    // auth_token is the dashboard admin token — encrypt at rest.
                    // A placeholder (`«set»`, `***set***`, `****…`) keeps the
                    // stored token; the plaintext key is only dropped once the
                    // encrypted copy is in the same commit.
                    if super::config_commit::is_secret_placeholder(v) {
                        // untouched — leave existing value
                    } else if v.is_empty() {
                        gateway.remove("auth_token");
                        gateway.remove("auth_token_enc");
                        changes.push("gateway.auth_token cleared (restart required)".into());
                    } else if v.len() < 16 {
                        return WsFrame::error_response(
                            "",
                            "gateway.auth_token must be at least 16 characters",
                        );
                    } else if let Some(enc) = crate::config_crypto::encrypt_value(v, &self.home_dir)
                    {
                        gateway.insert("auth_token_enc".into(), toml::Value::String(enc));
                        gateway.remove("auth_token");
                        changes.push("gateway.auth_token = [ENCRYPTED] (restart required)".into());
                    } else {
                        return WsFrame::error_response("", "Failed to encrypt gateway.auth_token");
                    }
                }
            }
        }

        // ── G.1b [gateway] allowed_origins (remote-access allowlist, hot-applied) ──
        // Array of remote dashboard origins (host / host:port / full URL). Each
        // entry is cleaned via the gateway's normalize step (scheme + trailing
        // slash stripped, empties dropped); the cleaned list is persisted and,
        // on a successful write, hot-applied via `set_allowed_origins` (which
        // re-merges the DUDUCLAW_ALLOWED_ORIGINS env) — no restart needed.
        if let Some(arr) = params.get("allowed_origins").and_then(|v| v.as_array()) {
            let cleaned: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter_map(crate::server::normalize_origin_entry)
                .collect();
            let gateway = table
                .entry("gateway")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            let toml_arr: Vec<toml::Value> = cleaned
                .iter()
                .map(|s| toml::Value::String(s.clone()))
                .collect();
            gateway.insert("allowed_origins".into(), toml::Value::Array(toml_arr));
            changes.push(format!(
                "gateway.allowed_origins = {} entr{}",
                cleaned.len(),
                if cleaned.len() == 1 { "y" } else { "ies" }
            ));
            applied_origins = Some(cleaned);
        }

        // ── G.2 [rotation] health_check_interval_seconds / cooldown_after_rate_limit_seconds ──
        {
            let has_rot = [
                "health_check_interval_seconds",
                "cooldown_after_rate_limit_seconds",
            ]
            .iter()
            .any(|k| params.get(*k).is_some());
            if has_rot {
                let rotation = table
                    .entry("rotation")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                for key in &[
                    "health_check_interval_seconds",
                    "cooldown_after_rate_limit_seconds",
                ] {
                    if let Some(v) = params.get(*key).and_then(|v| v.as_u64()) {
                        if v == 0 || v > 86400 {
                            return WsFrame::error_response(
                                "",
                                &format!("rotation.{key} must be 1-86400"),
                            );
                        }
                        rotation.insert((*key).into(), toml::Value::Integer(v as i64));
                        changes.push(format!("rotation.{key} = {v}"));
                    }
                }
            }
        }

        // ── G.3 [general] name / default_agent / inference_mode / default_language ──
        {
            let has_gen = [
                "name",
                "default_agent",
                "inference_mode",
                "default_language",
            ]
            .iter()
            .any(|k| params.get(*k).is_some());
            if has_gen {
                let general = table
                    .entry("general")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                // Gateway display name = the mDNS instance name shown in the
                // desktop picker. Empty clears it (falls back to hostname).
                if let Some(v) = params.get("name").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if v.len() > 64 {
                        return WsFrame::error_response("", "general.name must be ≤ 64 chars");
                    }
                    if v.is_empty() {
                        general.remove("name");
                        changes.push("general.name cleared".into());
                    } else {
                        general.insert("name".into(), toml::Value::String(v.into()));
                        changes.push(format!("general.name = \"{v}\""));
                    }
                }
                if let Some(v) = params.get("default_agent").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if !v.is_empty() && !is_valid_agent_id(v) {
                        return WsFrame::error_response("", "Invalid default_agent id");
                    }
                    general.insert("default_agent".into(), toml::Value::String(v.into()));
                    changes.push(format!("general.default_agent = \"{v}\""));
                }
                if let Some(v) = params.get("inference_mode").and_then(|v| v.as_str()) {
                    match v {
                        "local" | "claude" | "hybrid" => {
                            general.insert("inference_mode".into(), toml::Value::String(v.into()));
                            changes.push(format!("general.inference_mode = \"{v}\""));
                        }
                        _ => {
                            return WsFrame::error_response(
                                "",
                                "Invalid inference_mode. Valid: local, claude, hybrid",
                            );
                        }
                    }
                }
                // WP: global default reply language. Empty string clears it
                // (agent reverts to "follow the user's input language" — the
                // pre-existing behaviour). Not an enum: any BCP-47-ish tag is
                // accepted so operators aren't blocked on a code the
                // dashboard dropdown hasn't been updated to offer yet; the
                // prompt-injection side (`prompt_identity::language_instruction`)
                // degrades gracefully to the raw code for unrecognized values.
                if let Some(v) = params.get("default_language").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if v.is_empty() {
                        general.remove("default_language");
                        changes.push("general.default_language cleared".into());
                    } else {
                        general.insert("default_language".into(), toml::Value::String(v.into()));
                        changes.push(format!("general.default_language = \"{v}\""));
                    }
                }
            }
        }

        // ── G.3b [server] mdns_advertise (LAN discovery broadcast, restart required) ──
        // Read once at gateway start (server.rs) — persist + flag, never hot-apply.
        if let Some(v) = params.get("mdns_advertise").and_then(|v| v.as_bool()) {
            let server = table
                .entry("server")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            server.insert("mdns_advertise".into(), toml::Value::Boolean(v));
            changes.push(format!("server.mdns_advertise = {v} (restart required)"));
        }

        // ── G.3c [skills] gap_digest_enabled (daily skill-gap digest, hot-applied) ──
        // Re-read from config.toml on every digest tick (skill_gap_digest.rs),
        // so persisting is enough — no restart, no hot-reload plumbing.
        if let Some(v) = params.get("gap_digest_enabled").and_then(|v| v.as_bool()) {
            let skills = table
                .entry("skills")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(skills) = skills {
                skills.insert("gap_digest_enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("skills.gap_digest_enabled = {v}"));
            } else {
                return WsFrame::error_response("", "Invalid [skills] section in config.toml");
            }
        }

        // ── [memory] novelty_gate (B1 write-time near-duplicate rejection) ──
        // NOT hot-applied like gap_digest_enabled above: `mcp.rs::
        // novelty_gate_enabled_from_config` is read once, when a `duduclaw
        // mcp-server` process starts (`maybe_with_semantic_embedder`), and
        // cached in that process's `SqliteMemoryEngine` for its lifetime.
        // Persisting here is still correct — a NEW MCP server process (the
        // next agent session under the default fresh-spawn CLI runtime picks
        // this up on its very next turn) reads the new value — but an
        // already-running long-lived session (PTY-pool mode) will not see
        // the change until it restarts. Flagged in `changes` so the caller
        // can surface the same "restart/new-session" honesty the dashboard
        // needs instead of implying instant effect.
        if let Some(v) = params.get("novelty_gate_enabled").and_then(|v| v.as_bool()) {
            let memory = table
                .entry("memory")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(memory) = memory {
                memory.insert("novelty_gate".into(), toml::Value::Boolean(v));
                changes.push(format!(
                    "memory.novelty_gate = {v} (applies to new sessions)"
                ));
            } else {
                return WsFrame::error_response("", "Invalid [memory] section in config.toml");
            }
        }

        // ── S20 [miniapp] enabled (Telegram Mini App approval screen) ──
        // Until 2026-09 `[miniapp] enabled` had no dashboard surface at all, so
        // the feature was only reachable by hand-editing `config.toml`. The
        // routes read the key per request (`miniapp::enabled`), so persisting
        // it here takes effect on the very next request — no restart.
        if let Some(v) = params.get("miniapp_enabled").and_then(|v| v.as_bool()) {
            let miniapp = table
                .entry("miniapp")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(miniapp) = miniapp {
                miniapp.insert("enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("miniapp.enabled = {v}"));
            } else {
                return WsFrame::error_response("", "Invalid [miniapp] section in config.toml");
            }
        }

        // ── W2-8 [notify] daily_digest / daily_digest_at (dashboard toggle) ──
        // Re-read from config.toml on every `DailyDigestScheduler` tick
        // (`notify_digest.rs::DigestConfig::from_home`), so persisting is
        // enough — no restart, no hot-reload plumbing (same posture as
        // gap_digest_enabled above). `daily_digest_at` is validated with the
        // SAME parser the scheduler itself uses (`notify_digest::parse_clock`)
        // so a malformed time is rejected here rather than silently falling
        // back to 09:00 hours later when the scheduler's own fail-open read
        // path hits it.
        {
            let has_notify = ["daily_digest", "daily_digest_at"]
                .iter()
                .any(|k| params.get(*k).is_some());
            if has_notify {
                let notify = table
                    .entry("notify")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut();
                let Some(notify) = notify else {
                    return WsFrame::error_response("", "Invalid [notify] section in config.toml");
                };
                if let Some(v) = params.get("daily_digest").and_then(|v| v.as_bool()) {
                    notify.insert("daily_digest".into(), toml::Value::Boolean(v));
                    changes.push(format!("notify.daily_digest = {v}"));
                }
                if let Some(v) = params.get("daily_digest_at").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if crate::notify_digest::parse_clock(v).is_none() {
                        return WsFrame::error_response(
                            "",
                            &format!(
                                "Invalid notify.daily_digest_at '{v}' (need \"HH:MM\", e.g. \"09:00\")"
                            ),
                        );
                    }
                    notify.insert("daily_digest_at".into(), toml::Value::String(v.into()));
                    changes.push(format!("notify.daily_digest_at = \"{v}\""));
                }
            }
        }

        // ── G.4 [logging] format (pretty/json) ──
        if let Some(v) = params.get("log_format").and_then(|v| v.as_str()) {
            match v {
                "pretty" | "json" => {
                    let logging = table
                        .entry("logging")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .unwrap();
                    logging.insert("format".into(), toml::Value::String(v.into()));
                    changes.push(format!("logging.format = \"{v}\""));
                }
                _ => return WsFrame::error_response("", "Invalid log_format. Valid: pretty, json"),
            }
        }

        // ── G.7 [secret_manager] backend / vault_addr / vault_token(→_enc) / vault_mount ──
        if let Some(sm) = params.get("secret_manager").and_then(|v| v.as_object()) {
            let section = table
                .entry("secret_manager")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = sm.get("backend").and_then(|v| v.as_str()) {
                // Must match the real `SecretBackend` enum (secret_manager/mod.rs):
                // local | vault | env | onepassword | infisical. The prior list
                // ("config"/"keychain") named backends that do not exist and
                // rejected "local" (the default), so any valid selection failed.
                match v {
                    "local" | "vault" | "env" | "onepassword" | "infisical" => {
                        section.insert("backend".into(), toml::Value::String(v.into()));
                        changes.push(format!("secret_manager.backend = \"{v}\""));
                    }
                    _ => {
                        return WsFrame::error_response(
                            "",
                            "Invalid secret_manager.backend. Valid: local, vault, env, onepassword, infisical",
                        );
                    }
                }
            }
            // v1.68: every key the reader (`SecretManagerConfig`) knows is
            // accepted; anything else is refused by name instead of being
            // silently dropped while the rest of the payload saves.
            const PLAIN_KEYS: &[&str] = &[
                "vault_addr",
                "vault_mount",
                "onepassword_host",
                "onepassword_vault",
                "infisical_addr",
                "infisical_project_id",
                "infisical_environment",
            ];
            const SECRET_KEYS: &[&str] = &["vault_token", "onepassword_token", "infisical_token"];
            // A stored token only follows the address it was entered for:
            // changing the address while keeping the token (absent or a
            // placeholder) is refused, so the token is never sent to a host
            // chosen in this same edit.
            for (addr, token) in [
                ("vault_addr", "vault_token"),
                ("onepassword_host", "onepassword_token"),
                ("infisical_addr", "infisical_token"),
            ] {
                let new_addr = sm.get(addr).and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
                if new_addr.is_empty() || section.get(addr).and_then(|v| v.as_str()) == Some(new_addr) {
                    continue;
                }
                let stored_token = section.contains_key(&format!("{token}_enc")) || section.contains_key(token);
                let token_kept = sm
                    .get(token)
                    .and_then(|v| v.as_str())
                    .is_none_or(|t| super::config_commit::is_secret_placeholder(t.trim()));
                if stored_token && token_kept {
                    return WsFrame::error_response(
                        "",
                        &format!("secret_manager.{addr} changed — re-enter secret_manager.{token} for the new address"),
                    );
                }
            }
            if let Some(unknown) = sm
                .keys()
                .find(|k| k.as_str() != "backend" && !PLAIN_KEYS.contains(&k.as_str()) && !SECRET_KEYS.contains(&k.as_str()))
            {
                return WsFrame::error_response("", &format!("Unknown secret_manager key `{unknown}`"));
            }
            for key in PLAIN_KEYS {
                match sm.get(*key) {
                    None | Some(Value::Null) => {}
                    Some(Value::String(v)) => {
                        let v = v.trim();
                        if v.chars().any(char::is_control) || v.len() > 2048 {
                            return WsFrame::error_response("", &format!("secret_manager.{key} is not valid"));
                        }
                        if v.is_empty() {
                            section.remove(*key);
                            changes.push(format!("secret_manager.{key} cleared"));
                        } else {
                            section.insert((*key).into(), toml::Value::String(v.into()));
                            changes.push(format!("secret_manager.{key} = \"{v}\""));
                        }
                    }
                    Some(_) => {
                        return WsFrame::error_response("", &format!("secret_manager.{key} must be a string"));
                    }
                }
            }
            // Tokens → `<key>_enc` (G.7 / XC.5); never stored or echoed in
            // plaintext. A masked placeholder leaves the stored value alone.
            for key in SECRET_KEYS {
                let Some(raw) = sm.get(*key).filter(|v| !v.is_null()) else { continue };
                let Some(v) = raw.as_str().map(str::trim) else {
                    return WsFrame::error_response("", &format!("secret_manager.{key} must be a string"));
                };
                let enc_key = format!("{key}_enc");
                if super::config_commit::is_secret_placeholder(v) {
                    continue;
                }
                section.remove(*key);
                if v.is_empty() {
                    section.remove(&enc_key);
                    changes.push(format!("secret_manager.{key} cleared"));
                } else if let Some(enc) = crate::config_crypto::encrypt_value(v, &self.home_dir) {
                    section.insert(enc_key, toml::Value::String(enc));
                    changes.push(format!("secret_manager.{key} = [ENCRYPTED]"));
                } else {
                    return WsFrame::error_response(
                        "",
                        &format!("Failed to encrypt secret_manager.{key}"),
                    );
                }
            }
        }

        // ── v1.39 config knobs ─────────────────────────────────────────────
        // knowledge_guard / goal_loop / dispatch / memory / topology_evolution.
        // Sent as nested objects (like `secret_manager` / `voice`). Two classes:
        //   • "easy" per-use-read knobs (knowledge_guard.*, goal_loop.planner_enabled,
        //     memory.graph_embed_seed) — the consumer re-reads config.toml on every
        //     use, so the write alone takes effect; surfaced to the UI as `applied`.
        //   • "hard" startup-read knobs (goal_loop.iteration_cap_simple,
        //     dispatch.policy, topology_evolution.enabled) — a long-lived driver
        //     captured the value at boot, so we abort+respawn the driver after the
        //     write; surfaced as `hot_reloaded`.
        let mut applied_immediate = false;
        let mut reload_goal_loop = false;
        let mut reload_topology = false;
        let mut reload_dispatch = false;

        // [knowledge_guard] enabled / window_secs / max_per_subject
        if let Some(kg) = params.get("knowledge_guard").and_then(|v| v.as_object()) {
            let section = table
                .entry("knowledge_guard")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = kg.get("enabled").and_then(|v| v.as_bool()) {
                section.insert("enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("knowledge_guard.enabled = {v}"));
                applied_immediate = true;
            }
            if let Some(v) = kg.get("window_secs").and_then(|v| v.as_u64()) {
                if v == 0 || v > 604_800 {
                    return WsFrame::error_response(
                        "",
                        "knowledge_guard.window_secs must be 1-604800",
                    );
                }
                section.insert("window_secs".into(), toml::Value::Integer(v as i64));
                changes.push(format!("knowledge_guard.window_secs = {v}"));
                applied_immediate = true;
            }
            if let Some(v) = kg.get("max_per_subject").and_then(|v| v.as_u64()) {
                if v == 0 || v > 10_000 {
                    return WsFrame::error_response(
                        "",
                        "knowledge_guard.max_per_subject must be 1-10000",
                    );
                }
                section.insert("max_per_subject".into(), toml::Value::Integer(v as i64));
                changes.push(format!("knowledge_guard.max_per_subject = {v}"));
                applied_immediate = true;
            }
        }

        // [goal_loop] planner_enabled (easy) / iteration_cap_simple (hard) /
        // resume_on_restart (WP-E — boot-only read, neither hot-reloaded nor
        // "easy": `GoalLoopConfig::from_home` / `pause_inflight_on_restart`
        // are only consulted at gateway boot, never on a config hot-reload —
        // see `goal_loop.rs`'s own doc comment on that function. So this
        // write persists but deliberately does NOT set `applied_immediate`
        // or any `reload_*` flag, same posture as the G.1 gateway.bind/port
        // "restart required" fields above.
        if let Some(gl) = params.get("goal_loop").and_then(|v| v.as_object()) {
            let section = table
                .entry("goal_loop")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = gl.get("planner_enabled").and_then(|v| v.as_bool()) {
                section.insert("planner_enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("goal_loop.planner_enabled = {v}"));
                applied_immediate = true;
            }
            if let Some(v) = gl.get("iteration_cap_simple").and_then(|v| v.as_u64()) {
                if !(1..=20).contains(&v) {
                    return WsFrame::error_response(
                        "",
                        "goal_loop.iteration_cap_simple must be 1-20",
                    );
                }
                section.insert(
                    "iteration_cap_simple".into(),
                    toml::Value::Integer(v as i64),
                );
                changes.push(format!("goal_loop.iteration_cap_simple = {v} (hot reload)"));
                reload_goal_loop = true;
            }
            // Fail-closed whitelist: exactly "auto" or "pause", nothing else
            // — an unrecognized value must be rejected at write time here,
            // not silently degrade later at `ResumeOnRestart::from_str_lenient`
            // read time (that lenient fallback exists for hand-edited
            // config.toml, not for a value this RPC itself just accepted).
            if let Some(v) = gl.get("resume_on_restart").and_then(|v| v.as_str()) {
                match v {
                    "auto" | "pause" => {
                        section.insert("resume_on_restart".into(), toml::Value::String(v.into()));
                        changes.push(format!(
                            "goal_loop.resume_on_restart = \"{v}\" (takes effect on next gateway restart)"
                        ));
                    }
                    _ => {
                        return WsFrame::error_response(
                            "",
                            "Invalid goal_loop.resume_on_restart. Valid: auto, pause",
                        );
                    }
                }
            }
        }

        // [dispatch] enabled (hard — gates the dispatch engine + goal-loop
        // driver) / policy (hard — captured by the goal-loop driver at boot)
        if let Some(dp) = params.get("dispatch").and_then(|v| v.as_object()) {
            if let Some(v) = dp.get("enabled").and_then(|v| v.as_bool()) {
                let section = table
                    .entry("dispatch")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                section.insert("enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("dispatch.enabled = {v} (hot reload)"));
                reload_dispatch = true;
            }
            if let Some(v) = dp.get("policy").and_then(|v| v.as_str()) {
                // `role_team` is accepted by the reader
                // (`dispatch_policy.rs`) and was refused here until v1.68.
                match v {
                    "fixed_hierarchy" | "round_robin" | "llm_select" | "role_team" => {
                        let section = table
                            .entry("dispatch")
                            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                            .as_table_mut()
                            .unwrap();
                        section.insert("policy".into(), toml::Value::String(v.into()));
                        changes.push(format!("dispatch.policy = \"{v}\" (hot reload)"));
                        reload_goal_loop = true;
                    }
                    _ => {
                        return WsFrame::error_response(
                            "",
                            "Invalid dispatch.policy. Valid: fixed_hierarchy, round_robin, llm_select, role_team",
                        );
                    }
                }
            }
            // WP-5D judge seam: `[dispatch] judge` (easy — `review_goal_tasks`
            // re-reads it per reviewed task, exactly like `two_stage_judge`, so
            // no driver respawn is needed for the switch to take effect).
            //
            // Deliberately NOT settable here: `judge_command` /
            // `judge_timeout_secs`. Those name an executable, and this RPC is
            // reachable from the dashboard; keeping them file-only (where
            // `org_field_guard` already DENIES agent writes to
            // `<home>/config.toml`) means no dashboard or agent path can point
            // the judge seam at an arbitrary binary. Value set is enumerated
            // (`JudgeMode::from_config_str`); only `mav` and `external` may be
            // written. The values removed in v1.69.0 (`evaluator_only`,
            // `human_only` and their aliases) are refused with an end-user
            // message naming the replacement, and the stored value is left
            // untouched. An unknown value is refused too (the read path
            // additionally falls back to `mav`, the strongest verifier).
            if let Some(v) = dp.get("judge").and_then(|v| v.as_str()) {
                match crate::judge_mode::JudgeMode::from_config_str(v) {
                    Some(mode) if mode.is_removed() => {
                        warn!(
                            value = mode.as_str(),
                            "system.update_config refused a removed dispatch.judge value"
                        );
                        return WsFrame::error_response(
                            "",
                            crate::judge_mode::removed_mode_write_error(mode)
                                .unwrap_or("這個驗收方式已移除，請改選「標準驗收」。"),
                        );
                    }
                    Some(mode) => {
                        let section = table
                            .entry("dispatch")
                            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                            .as_table_mut()
                            .unwrap();
                        section.insert("judge".into(), toml::Value::String(mode.as_str().into()));
                        changes.push(format!(
                            "dispatch.judge = \"{}\" (hot reload)",
                            mode.as_str()
                        ));
                    }
                    None => {
                        return WsFrame::error_response(
                            "",
                            "Invalid dispatch.judge. Valid: mav, external",
                        );
                    }
                }
            }
        }

        // [memory] graph_embed_seed (easy — re-read on every memory RPC)
        if let Some(mem) = params.get("memory").and_then(|v| v.as_object()) {
            if let Some(v) = mem.get("graph_embed_seed").and_then(|v| v.as_bool()) {
                let section = table
                    .entry("memory")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                section.insert("graph_embed_seed".into(), toml::Value::Boolean(v));
                changes.push(format!("memory.graph_embed_seed = {v}"));
                applied_immediate = true;
            }
        }

        // [topology_evolution] enabled (hard — gates the D5 driver at boot)
        if let Some(te) = params.get("topology_evolution").and_then(|v| v.as_object()) {
            if let Some(v) = te.get("enabled").and_then(|v| v.as_bool()) {
                let section = table
                    .entry("topology_evolution")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                section.insert("enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("topology_evolution.enabled = {v} (hot reload)"));
                reload_topology = true;
            }
        }

        // [belief] flat_band_pct (easy — `BeliefConfig::from_db_path` re-reads
        // config.toml on every `belief::settle` call) / tick_subject_map (easy
        // — read by the autopilot tick-wake hook on every tick).
        if let Some(bl) = params.get("belief").and_then(|v| v.as_object()) {
            let section = table
                .entry("belief")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = bl.get("flat_band_pct").and_then(|v| v.as_f64()) {
                if !(0.01..=10.0).contains(&v) {
                    return WsFrame::error_response("", "belief.flat_band_pct must be 0.01-10.0");
                }
                section.insert("flat_band_pct".into(), toml::Value::Float(v));
                changes.push(format!("belief.flat_band_pct = {v}"));
                applied_immediate = true;
            }
            if let Some(map) = bl.get("tick_subject_map").and_then(|v| v.as_object()) {
                if map.len() > 32 {
                    return WsFrame::error_response(
                        "",
                        "belief.tick_subject_map supports at most 32 entries",
                    );
                }
                let mut tsm_table = toml::map::Map::new();
                for (k, v) in map {
                    if k.trim().is_empty() || k.chars().count() > 64 {
                        return WsFrame::error_response(
                            "",
                            "belief.tick_subject_map keys must be non-empty and <= 64 chars",
                        );
                    }
                    let Some(v_str) = v.as_str() else {
                        return WsFrame::error_response(
                            "",
                            "belief.tick_subject_map values must be strings",
                        );
                    };
                    if v_str.trim().is_empty() || v_str.chars().count() > 64 {
                        return WsFrame::error_response(
                            "",
                            "belief.tick_subject_map values must be non-empty and <= 64 chars",
                        );
                    }
                    tsm_table.insert(k.clone(), toml::Value::String(v_str.to_string()));
                }
                section.insert("tick_subject_map".into(), toml::Value::Table(tsm_table));
                changes.push(format!("belief.tick_subject_map = {} entries", map.len()));
                applied_immediate = true;
            }
        }

        // ── v1.68 contract keys (takeover / mail / webchat / tick / files /
        // night / judge model / acp / telemetry / container.sandbox /
        // computer_use / memory guard / team / github / redaction purge) ──
        let v168 = match super::system_update_config_v168::apply_v168_keys(&mut table, &params) {
            Ok(o) => o,
            Err(e) => return WsFrame::error_response("", &e),
        };
        changes.extend(v168.changes.iter().cloned());
        restart_required.extend(v168.restart_required.iter().cloned());
        if v168.applied_immediate {
            applied_immediate = true;
        }
        for (flat, key) in super::system_update_config_v168::LEGACY_RESTART_KEYS {
            if params.get(*flat).is_some() && changes.iter().any(|c| c.starts_with(*key)) {
                restart_required.push((*key).to_string());
            }
        }
        if super::config_commit::param_at(&params, "goal_loop.resume_on_restart").is_some() {
            restart_required.push("goal_loop.resume_on_restart".into());
        }
        let rotation_cache_dirty = params.get("rotation_strategy").is_some()
            || params.get("cooldown_after_rate_limit_seconds").is_some();

        // ── voice (persisted to inference.toml [voice], where VoiceConfig reads it) ──
        // Track how many config.toml changes were accumulated BEFORE the voice
        // block so the early-return below stays correct for mixed payloads.
        // v1.68: `asr_provider` / `asr_language` / `voice_reply_enabled` had no
        // reader and are no longer accepted (ignored if an old client sends
        // them); speech-to-text lives in `config.toml [voice] stt_*`.
        let config_toml_changes = changes.len();
        if let Some(voice) = params.get("voice").and_then(|v| v.as_object()) {
            const VALID_TTS: &[&str] = &["auto", "edge-tts", "minimax", "openai-tts", "piper"];

            let inference_path = self.home_dir.join("inference.toml");
            let mut inf_table = self.read_config_table(&inference_path).await;
            let mut voice_dirty = false;
            let voice_table = inf_table
                .entry("voice")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(voice_table) = voice_table {
                if let Some(v) = voice.get("tts_provider").and_then(|v| v.as_str()) {
                    if !VALID_TTS.contains(&v) {
                        return WsFrame::error_response(
                            "",
                            &format!(
                                "Invalid tts_provider '{v}'. Valid: {}",
                                VALID_TTS.join(", ")
                            ),
                        );
                    }
                    voice_table.insert("tts_provider".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
                if let Some(v) = voice.get("tts_voice").and_then(|v| v.as_str()) {
                    voice_table.insert("tts_voice".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
            }

            if voice_dirty {
                let tmp = inference_path.with_extension("toml.tmp");
                if let Err(e) = self.write_config_table(&tmp, &inf_table).await {
                    return WsFrame::error_response(
                        "",
                        &format!("Failed to write inference.toml: {e}"),
                    );
                }
                if let Err(e) = tokio::fs::rename(&tmp, &inference_path).await {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return WsFrame::error_response(
                        "",
                        &format!("Failed to commit inference.toml: {e}"),
                    );
                }
                changes.push("voice (inference.toml)".to_string());
            }

            // `voice` may be the only effective field in the payload;
            // config.toml itself is untouched in that case, so return early
            // before the config.toml write below complains about
            // "no valid fields".
            if config_toml_changes == 0 && !changes.is_empty() {
                info!(?changes, "system.update_config completed");
                duduclaw_security::audit::log_config_changed(
                    &self.home_dir,
                    &ctx.email,
                    &format!("{:?}", ctx.role).to_lowercase(),
                    &changes,
                );
                // C1 producer 甲 companion — see `security_autopilot.rs`.
                crate::security_autopilot::emit_config_changed();
                return WsFrame::ok_response(
                    "",
                    json!({ "success": true, "changes": changes, "restart_required": [] }),
                );
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response(
                "",
                "No valid fields to update. Supported: log_level, log_format, rotation_strategy, auto_update, voice, allowed_origins, gateway(bind/port/auth_token), rotation(health_check_interval_seconds/cooldown_after_rate_limit_seconds), general(default_agent/inference_mode/default_language), secret_manager, knowledge_guard(enabled/window_secs/max_per_subject), goal_loop(planner_enabled/iteration_cap_simple/resume_on_restart), dispatch(enabled/policy), memory(graph_embed_seed), topology_evolution(enabled), belief(flat_band_pct/tick_subject_map), takeover, mail, webchat, tick, files.allowed_roots, night.llm_enabled, dispatch(judge_provider/judge_model), acp.trusted, telemetry.otlp_endpoint, container.sandbox, computer_use.image, memory.supersession_trust_guard, team, integrations.github, redaction.purge_after_expire_days",
            );
        }

        // Atomic write (temp + rename) under the cross-process config lock,
        // refused when another writer changed the file since it was read.
        if let Err(e) =
            super::config_commit::commit_table_locked(&config_path, original_hash, &table).await
        {
            return WsFrame::error_response("", &e);
        }

        // v1.68: security-relevant keys get their own audit row with the
        // before/after value (on top of the generic `config_changed` below).
        for p in &v168.protected {
            crate::security_autopilot::audit_and_emit(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "config_protected_key_changed",
                    &ctx.user_id,
                    duduclaw_security::audit::Severity::Warning,
                    json!({
                        "key": p.key,
                        "before": p.before,
                        "after": p.after,
                        "user_id": ctx.user_id,
                        "source": "system.update_config",
                    }),
                ),
            );
        }
        // `[rotation] strategy` / `cooldown_after_rate_limit_seconds` are read
        // when the account rotator is built; drop the cached one so the next
        // call rebuilds it (otherwise up to a 30-minute lag).
        if rotation_cache_dirty {
            crate::claude_runner::invalidate_rotator_cache().await;
        }

        // Hot-apply the remote-access allowlist so the dashboard save takes
        // effect immediately — no gateway restart. Only reached once the config
        // write above committed successfully.
        let origins_applied = if let Some(cleaned) = applied_origins {
            crate::server::set_allowed_origins(cleaned);
            true
        } else {
            false
        };

        // Hot reload the long-lived background drivers whose config was captured
        // at boot. abort+respawn with the freshly-written config (drivers are
        // stateless periodic pollers — durable state lives in SQLite, so an abort
        // between ticks is safe). Report which reloaded so the UI can confirm.
        let mut hot_reloaded: Vec<&'static str> = Vec::new();
        if reload_dispatch {
            // Order matters: the engine (re)constructs the shared forward-model
            // Arc when `[task_forward_model]` is enabled, and the goal-loop
            // driver respawn below picks that same Arc up for its predict hook.
            self.respawn_dispatch_engine().await;
            self.respawn_goal_loop_driver().await;
            hot_reloaded.push("dispatch");
        }
        if reload_goal_loop && !reload_dispatch {
            self.respawn_goal_loop_driver().await;
        }
        if reload_goal_loop {
            hot_reloaded.push("goal_loop");
        }
        if reload_topology {
            self.respawn_topology_driver().await;
            hot_reloaded.push("topology_evolution");
        }
        // WP-S: `[general] log_level` applies to the running logger (reload
        // handle in `crate::log`) unless RUST_LOG pins the level.
        if let Some(level) = params.get("log_level").and_then(|v| v.as_str()) {
            if crate::log::apply_log_level(level) == Ok(crate::log::LogLevelApply::Applied) {
                hot_reloaded.push("log_level");
            } else {
                // RUST_LOG pins the level, or no reload handle is installed.
                restart_required.push("general.log_level".into());
            }
        }
        if rotation_cache_dirty {
            hot_reloaded.push("rotation");
        }
        // `[tick]`: respawn the source tasks with the new config when the
        // tick runtime is installed; otherwise the change waits for a restart.
        if v168.reload_ticks {
            if self.respawn_tick_sources().await.is_some() {
                hot_reloaded.push("tick");
            } else {
                restart_required.push("tick".into());
            }
        }
        // `[redaction] purge_after_expire_days` is taken by the vault GC when
        // the pipeline is (re)built, so rebuild it the way `redaction.update`
        // does.
        if v168.reload_redaction {
            let (applied, _warning) = self.apply_redaction_hot_reload(&table).await;
            if applied {
                hot_reloaded.push("redaction");
            } else {
                restart_required.push("redaction.purge_after_expire_days".into());
            }
        }
        restart_required.sort();
        restart_required.dedup();

        info!(?changes, ?hot_reloaded, "system.update_config completed");
        // B5 (OS security line P0): every accepted `config.toml` write is now
        // auditable — same shape/spirit as `delegation.set`'s pre-existing
        // `delegation_config_changed` event below. Fired only here (and at
        // the voice-only early-return above), i.e. only once the write is
        // durably committed — a rejected/failed update never reaches this
        // point.
        duduclaw_security::audit::log_config_changed(
            &self.home_dir,
            &ctx.email,
            &format!("{:?}", ctx.role).to_lowercase(),
            &changes,
        );
        // C1 producer 甲 companion — see `security_autopilot.rs`.
        crate::security_autopilot::emit_config_changed();
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "changes": changes,
                // Signals the UI that a live-applied field took effect without a
                // restart: allowed_origins hot-apply OR an "easy" per-use-read knob.
                "applied": origins_applied || applied_immediate,
                // Drivers that were abort+respawned with the new config.
                "hot_reloaded": hot_reloaded,
                // v1.68: TOML keys (or a whole section, e.g. "tick") whose
                // new value only takes effect after a gateway restart.
                "restart_required": restart_required,
            }),
        )
    }
}
