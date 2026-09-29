use super::*;

pub(crate) const VALID_CHANNELS: &[&str] = duduclaw_gateway::channel_settings::VALID_CHANNEL_TYPES;

pub(crate) fn valid_config_key(key: &str) -> bool {
    duduclaw_gateway::channel_settings::CONFIG_KEYS.contains(&key)
        || duduclaw_gateway::channel_settings::MCP_ACCESS_KEYS.contains(&key)
}

pub(crate) fn valid_keys_help() -> String {
    duduclaw_gateway::channel_settings::CONFIG_KEYS
        .iter()
        .chain(duduclaw_gateway::channel_settings::MCP_ACCESS_KEYS.iter())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn validate_scope_id(scope_id: &str) -> std::result::Result<(), String> {
    duduclaw_gateway::channel_settings::validate_scope_id(scope_id)
}

pub(crate) fn validate_value(key: &str, value: &str) -> std::result::Result<(), String> {
    duduclaw_gateway::channel_settings::validate_setting_value(key, value)
}

pub(crate) async fn handle_channel_config(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return tool_error("Missing required parameter: key"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if !valid_config_key(key) {
        return tool_error(&format!(
            "Invalid key: {key}. Valid keys: {}",
            valid_keys_help()
        ));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr =
        match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path)
        {
            Ok(m) => m,
            Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
        };

    if let Some(value) = args.get("value").and_then(|v| v.as_str()) {
        if let Err(e) = validate_value(key, value) {
            return tool_error(&format!("Invalid value: {e}"));
        }
        match mgr.set(channel, scope_id, key, value).await {
            Ok(()) => tool_text(&format!("Set {channel}/{scope_id}/{key} = {value}")),
            Err(e) => tool_error(&format!("Failed to set: {e}")),
        }
    } else {
        let value = mgr
            .get_with_fallback(channel, scope_id, key, "(not set)")
            .await;
        tool_text(&format!("{channel}/{scope_id}/{key} = {value}"))
    }
}

pub(crate) async fn handle_channel_config_list(args: &Value, home_dir: &Path) -> Value {
    let channel = match args.get("channel").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: channel"),
    };
    let scope_id = match args.get("scope_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: scope_id"),
    };

    if !VALID_CHANNELS.contains(&channel) {
        return tool_error(&format!("Invalid channel type: {channel}"));
    }
    if let Err(e) = validate_scope_id(scope_id) {
        return tool_error(&format!("Invalid scope_id: {e}"));
    }

    let db_path = home_dir.join("sessions.db");
    let mgr =
        match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path)
        {
            Ok(m) => m,
            Err(e) => return tool_error(&format!("Failed to open settings DB: {e}")),
        };

    let all = mgr.get_all(channel, scope_id).await;
    if all.is_empty() {
        tool_text(&format!(
            "No settings configured for {channel}/{scope_id}. Using defaults."
        ))
    } else {
        let lines: Vec<String> = all.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        tool_text(&format!(
            "Settings for {channel}/{scope_id}:\n{}",
            lines.join("\n")
        ))
    }
}

pub(crate) async fn handle_channel_status(args: &Value, home_dir: &Path) -> Value {
    let filter = args
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(f) = &filter {
        if !VALID_CHANNELS.contains(&f.as_str()) {
            return tool_error(&format!("Invalid channel type: {f}"));
        }
    }

    // 1. Connection snapshot — persisted by the gateway on every status change
    //    (`channel_status.json`); absent when the gateway has never run.
    let snapshot_path = home_dir.join("channel_status.json");
    let connections: Value = std::fs::read_to_string(&snapshot_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "channels": {},
                "note": "no gateway snapshot found — is the gateway running?"
            })
        });

    // 2. Session counts from sessions.db (read-only; missing DB = empty stats).
    let db_path = home_dir.join("sessions.db");
    let session_stats = tokio::task::spawn_blocking(move || {
        let mut stats: std::collections::BTreeMap<String, ChannelSessionStats> = Default::default();
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return stats;
        };
        let Ok(mut stmt) = conn.prepare("SELECT id, last_active FROM sessions") else {
            return stats;
        };
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        if let Ok(rows) = rows {
            for (id, last_active) in rows.flatten() {
                let channel = id.split(':').next().unwrap_or("unknown").to_string();
                let entry = stats.entry(channel).or_default();
                entry.total_sessions += 1;
                if chrono::DateTime::parse_from_rfc3339(&last_active)
                    .map(|t| t.with_timezone(&chrono::Utc) > cutoff)
                    .unwrap_or(false)
                {
                    entry.active_24h += 1;
                }
                // Thread/topic sessions: `discord:thread:{id}` and
                // `telegram:{chat}:{topic}` (three segments).
                let is_thread = id.starts_with("discord:thread:")
                    || (id.starts_with("telegram:") && id.split(':').count() >= 3);
                if is_thread {
                    entry.thread_sessions += 1;
                }
            }
        }
        stats
    })
    .await
    .unwrap_or_default();

    // 3. Known Discord guilds + their per-guild settings (seeded on GUILD_CREATE).
    let guilds: Vec<Value> = if filter.as_deref().is_none_or(|f| f == "discord") {
        let db_path = home_dir.join("sessions.db");
        match duduclaw_gateway::channel_settings::ChannelSettingsManager::from_session_db(&db_path)
        {
            Ok(mgr) => {
                let mut out = Vec::new();
                for scope in mgr.list_scopes("discord").await {
                    if scope == "dm" {
                        continue;
                    }
                    let settings: serde_json::Map<String, Value> = mgr
                        .get_all("discord", &scope)
                        .await
                        .into_iter()
                        .map(|(k, v)| (k, Value::String(v)))
                        .collect();
                    out.push(serde_json::json!({ "guild_id": scope, "settings": settings }));
                }
                out
            }
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // Assemble, applying the optional channel filter to the session stats and
    // connection snapshot (connection labels may be "discord:{agent}" etc.).
    let sessions: serde_json::Map<String, Value> = session_stats
        .into_iter()
        .filter(|(ch, _)| filter.as_deref().is_none_or(|f| ch == f))
        .map(|(ch, st)| (ch, serde_json::to_value(st).unwrap_or_default()))
        .collect();
    let connections_filtered = match (
        &filter,
        connections.get("channels").and_then(|c| c.as_object()),
    ) {
        (Some(f), Some(map)) => {
            let filtered: serde_json::Map<String, Value> = map
                .iter()
                .filter(|(label, _)| *label == f || label.starts_with(&format!("{f}:")))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            serde_json::json!({ "channels": filtered, "updated_at": connections.get("updated_at") })
        }
        _ => connections,
    };

    let report = serde_json::json!({
        "connections": connections_filtered,
        "sessions": sessions,
        "discord_guilds": guilds,
    });
    tool_text(&serde_json::to_string_pretty(&report).unwrap_or_default())
}

// ── User pairing management ──────────────────────────────────────

pub(crate) async fn handle_pairing_manage(args: &Value, home_dir: &Path) -> Value {
    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return tool_error("Missing required parameter: action"),
    };
    let subject = args.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    if action != "list" && subject.is_empty() {
        return tool_error("Missing required parameter: subject (user id or session id)");
    }

    // Shares state with the gateway via ~/.duduclaw/access_control.json —
    // codes generated here are verifiable by the gateway's /pair handler.
    let ctrl = duduclaw_gateway::access_control::AccessController::with_persistence(
        home_dir.join("access_control.json"),
    );

    match action {
        "generate" => match ctrl.generate_pairing_code(subject).await {
            Some(code) => tool_text(&format!(
                "配對碼：{code}（5 分鐘內有效）。請使用者在頻道輸入：/pair {code}\nsubject: {subject}"
            )),
            None => tool_error("此 subject 的失敗次數過多，已鎖定產碼（防暴力破解上限 15 次）"),
        },
        "approve" => {
            ctrl.approve_user(subject).await;
            tool_text(&format!("已核准：{subject}"))
        }
        "revoke" => {
            ctrl.revoke_user(subject).await;
            tool_text(&format!("已撤銷：{subject}"))
        }
        "list" => {
            let users = ctrl.runtime_approved_users().await;
            if users.is_empty() {
                tool_text("目前沒有已核准的 subject。")
            } else {
                tool_text(&format!(
                    "已核准 {} 個 subject：\n{}",
                    users.len(),
                    users.join("\n")
                ))
            }
        }
        other => tool_error(&format!(
            "Unknown action: {other}. Valid: generate, approve, revoke, list"
        )),
    }
}

// ── Web fetch / extract handlers (browser pipeline L1 + L2) ─────
