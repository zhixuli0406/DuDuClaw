//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Validate + write the `[capabilities]` section into an agent.toml table from
/// the `agents.update` params. Returns the human-readable change list (may be
/// empty if no capability fields were present). Errors on invalid enum / range.
pub(crate) fn apply_capabilities_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    // Only act if the payload actually carries a `capabilities` object — this
    // keeps `agents.update` calls that don't touch capabilities clean.
    let cap = match params.get("capabilities").and_then(|v| v.as_object()) {
        Some(c) => c,
        None => return Ok(changes),
    };

    let section = table
        .entry("capabilities")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [capabilities] section".to_string())?;

    // ── Scalars ──
    if let Some(v) = cap.get("computer_use").and_then(|v| v.as_bool()) {
        section.insert("computer_use".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.computer_use = {v}"));
    }
    if let Some(v) = cap.get("computer_use_mode").and_then(|v| v.as_str()) {
        match v {
            "container" | "native" | "auto" => {
                section.insert("computer_use_mode".into(), toml::Value::String(v.into()));
                changes.push(format!("capabilities.computer_use_mode = \"{v}\""));
            }
            _ => {
                return Err(format!(
                    "Invalid computer_use_mode '{v}'. Valid: container, native, auto"
                ));
            }
        }
    }
    if let Some(v) = cap.get("browser_via_bash").and_then(|v| v.as_bool()) {
        section.insert("browser_via_bash".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.browser_via_bash = {v}"));
    }
    // ── os_native (bool) — opt-in OS-native features (filesystem watchers) ──
    // The actual `[os_watch]` paths live in a top-level table written by
    // `apply_os_watch_to_table`; this flag gates whether the watcher runs.
    if let Some(v) = cap.get("os_native").and_then(|v| v.as_bool()) {
        section.insert("os_native".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.os_native = {v}"));
    }
    // ── recording (bool) — opt-in recording-to-skill capture (WP3.3). Gates
    // the browser/desktop record + skill_from_recording MCP tools at the
    // dispatch gate (fail-closed, default false).
    if let Some(v) = cap.get("recording").and_then(|v| v.as_bool()) {
        section.insert("recording".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.recording = {v}"));
    }
    // ── git_credentials (bool) — opt-in hand-off of the operator's own
    // SSH/GPG identity to this agent's spawned CLI subprocess (WP-10A).
    // Default false ⇒ `duduclaw_core::spawn_env`'s WP-8B credential scrub
    // applies unchanged. Dashboard-facing wiring only (this WP); the actual
    // spawn-time env grant + audit logging already exist from WP-10A.
    if let Some(v) = cap.get("git_credentials").and_then(|v| v.as_bool()) {
        section.insert("git_credentials".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.git_credentials = {v}"));
    }
    // ── system_operator (bool) — opt-in system-operator designation (O-4).
    // Master switch for the `os_*` system-operation MCP tool face
    // (device/system status, backup, power, update, factory reset, doctor);
    // default false ⇒ denied at the dispatch gate even for an Admin-scoped
    // agent. A materially higher trust tier than `os_native` (which only
    // covers an agent's own host automation footprint), so this is a
    // dashboard-facing danger-zone toggle like `git_credentials` above —
    // same bool-write pattern, one field.
    if let Some(v) = cap.get("system_operator").and_then(|v| v.as_bool()) {
        section.insert("system_operator".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.system_operator = {v}"));
    }
    // ── codrive (bool) — opt-in human-machine co-drive (CD-1). Master
    // switch for the `codrive_run` MCP tool (GUI mouse/keyboard injection
    // via the duduclaw-comp compositor); default false ⇒ denied at the
    // dispatch gate even for an Admin-scoped agent. Same danger-zone
    // bool-write pattern as `git_credentials` / `system_operator` above.
    if let Some(v) = cap.get("codrive").and_then(|v| v.as_bool()) {
        section.insert("codrive".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.codrive = {v}"));
    }
    // ── autonomy_level (string) — how much the autonomous goal loop may
    // drive this agent on its own (`goal_loop::AutonomyLevel`). Not a typed
    // `CapabilitiesConfig` field — read straight from this raw TOML key by
    // `AutonomyLevel::for_agent`, same additive-gate convention as
    // `approval_required_tools`. Validated against the exact lowercase set
    // that parser recognizes so a typo can never silently land as the
    // conservative default instead of the level the operator picked
    // (fail-closed: reject, don't guess).
    if let Some(v) = cap.get("autonomy_level").and_then(|v| v.as_str()) {
        match v {
            "operator" | "collaborator" | "consultant" | "approver" | "observer" => {
                section.insert("autonomy_level".into(), toml::Value::String(v.into()));
                changes.push(format!("capabilities.autonomy_level = \"{v}\""));
            }
            _ => {
                return Err(format!(
                    "Invalid autonomy_level '{v}'. Valid: operator, collaborator, consultant, approver, observer"
                ));
            }
        }
    }

    // ── Array fields (tool names must be non-empty strings) ──
    for (param_key, toml_key) in &[
        ("allowed_tools", "allowed_tools"),
        ("denied_tools", "denied_tools"),
        ("wiki_visible_to", "wiki_visible_to"),
        // v1.68: approval / irreversibility / task-scoped grant lists (read
        // per call by `approval/gates.rs` and `capability_grants.rs`).
        ("approval_required_tools", "approval_required_tools"),
        ("irreversible_tools", "irreversible_tools"),
        ("maybe_irreversible_tools", "maybe_irreversible_tools"),
        ("scoped_tools", "scoped_tools"),
    ] {
        if let Some(arr) = cap.get(*param_key).and_then(|v| v.as_array()) {
            let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("capabilities.{param_key} entries must be strings"))?;
                let s = s.trim();
                if s.is_empty() {
                    return Err(format!(
                        "capabilities.{param_key} entries must be non-empty"
                    ));
                }
                out.push(toml::Value::String(s.into()));
            }
            section.insert((*toml_key).into(), toml::Value::Array(out));
            changes.push(format!("capabilities.{toml_key} = [{} entries]", arr.len()));
        }
    }

    // ── db_sources (WP-A) — the read-only SQL sources this agent may query ──
    // REPLACE semantics like its array siblings above (an empty array revokes
    // every grant), but normalized through the single shared helper
    // (trim + de-duplicate, first-seen order preserved) so the list that is
    // *validated* in `handle_agents_update` and the list that is *written*
    // here can never drift. The "is this a configured source?" check cannot
    // live in this function — it is sync and has no `home_dir` — so it runs
    // fail-closed in `handle_agents_update` before this closure is reached.
    if let Some(raw) = cap.get("db_sources") {
        let list = crate::db_source_grants::normalize_grant_list(raw)?;
        let n = list.len();
        section.insert(
            "db_sources".into(),
            toml::Value::Array(list.into_iter().map(toml::Value::String).collect()),
        );
        changes.push(format!("capabilities.db_sources = [{n} entries]"));
    }

    // ── action_rules (2026-10) — allow / ask / block per effect or tool ──
    // REPLACE semantics like the arrays above (an empty array removes every
    // rule). Validated strictly here so the dashboard can never write a rule
    // the gate would read as malformed (which it would treat fail-closed as
    // `ask` for every side effect): each entry is `{effect, verdict}` or
    // `{tool, verdict}` with known tokens and no other keys. Changing it is
    // an authority change (`[capabilities]` is compared whole by
    // `agents_update_v168::AUTHORITY_KEYS`).
    if let Some(raw) = cap.get("action_rules") {
        let arr = raw
            .as_array()
            .ok_or_else(|| "capabilities.action_rules must be an array".to_string())?;
        let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let obj = item
                .as_object()
                .ok_or_else(|| format!("capabilities.action_rules[{i}] must be an object"))?;
            if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "effect" | "tool" | "verdict")) {
                return Err(format!("capabilities.action_rules[{i}] has an unknown key '{k}'"));
            }
            let verdict: duduclaw_core::ActionVerdict = obj
                .get("verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .parse()
                .map_err(|_| format!("capabilities.action_rules[{i}].verdict must be one of: allow, ask, block"))?;
            let mut rule = toml::map::Map::new();
            match (obj.get("effect"), obj.get("tool")) {
                (Some(e), None) => {
                    let effect: duduclaw_core::ToolEffect = e.as_str().unwrap_or("").parse().map_err(|_| {
                        format!(
                            "capabilities.action_rules[{i}].effect must be one of: read, draft, send, publish, purchase, delete, modify, admin"
                        )
                    })?;
                    rule.insert("effect".into(), toml::Value::String(effect.as_str().into()));
                }
                (None, Some(t)) => {
                    let tool = t
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| format!("capabilities.action_rules[{i}].tool must be a non-empty string"))?;
                    rule.insert("tool".into(), toml::Value::String(tool.into()));
                }
                _ => {
                    return Err(format!(
                        "capabilities.action_rules[{i}] must name exactly one of effect or tool"
                    ));
                }
            }
            rule.insert("verdict".into(), toml::Value::String(verdict.as_str().into()));
            out.push(toml::Value::Table(rule));
        }
        let n = out.len();
        section.insert("action_rules".into(), toml::Value::Array(out));
        changes.push(format!("capabilities.action_rules = [{n} rules]"));
    }

    // ── trusted_read_hint_servers (2026-10-08) — third-party servers whose
    // `readOnlyHint` the tool gate believes. REPLACE semantics; each entry
    // must be a valid `.mcp.json` server name (not DuDuClaw's own).
    if let Some(raw) = cap.get("trusted_read_hint_servers") {
        let arr = raw
            .as_array()
            .ok_or_else(|| "capabilities.trusted_read_hint_servers must be an array".to_string())?;
        let mut out: Vec<String> = Vec::with_capacity(arr.len());
        for (i, v) in arr.iter().enumerate() {
            let name = v.as_str().map(str::trim).unwrap_or("");
            if !crate::mcp_scan::is_valid_mcp_server_name(name)
                || duduclaw_core::mcp_proxy_rewrite::is_duduclaw_server(name)
            {
                return Err(format!(
                    "capabilities.trusted_read_hint_servers[{i}] must be a third-party MCP server name"
                ));
            }
            if !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
        let n = out.len();
        section.insert(
            "trusted_read_hint_servers".into(),
            toml::Value::Array(out.into_iter().map(toml::Value::String).collect()),
        );
        changes.push(format!("capabilities.trusted_read_hint_servers = [{n} servers]"));
    }

    // ── [capabilities.computer_use_config] sub-table ──
    if let Some(cfg) = cap.get("computer_use_config").and_then(|v| v.as_object()) {
        let sub = section
            .entry("computer_use_config")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "Invalid [capabilities.computer_use_config] section".to_string())?;

        for (param_key, toml_key) in &[
            ("allowed_apps", "allowed_apps"),
            ("blocked_actions", "blocked_actions"),
        ] {
            if let Some(arr) = cfg.get(*param_key).and_then(|v| v.as_array()) {
                let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
                for item in arr {
                    let s = item.as_str().ok_or_else(|| {
                        format!("computer_use_config.{param_key} entries must be strings")
                    })?;
                    let s = s.trim();
                    if s.is_empty() {
                        return Err(format!(
                            "computer_use_config.{param_key} entries must be non-empty"
                        ));
                    }
                    out.push(toml::Value::String(s.into()));
                }
                sub.insert((*toml_key).into(), toml::Value::Array(out));
                changes.push(format!(
                    "capabilities.computer_use_config.{toml_key} = [{} entries]",
                    arr.len()
                ));
            }
        }

        // Navigation allowlist for tool-driven sessions: REPLACE semantics,
        // every entry must be an exact hostname (the reader would drop it
        // anyway; refusing here tells the operator at save time).
        if let Some(arr) = cfg.get("allowed_domains").and_then(|v| v.as_array()) {
            let mut hosts: Vec<String> = Vec::with_capacity(arr.len());
            for item in arr {
                let raw = item.as_str().ok_or_else(|| {
                    "computer_use_config.allowed_domains entries must be strings".to_string()
                })?;
                let host = duduclaw_core::types::normalize_navigation_host(raw).ok_or_else(|| {
                    format!(
                        "computer_use_config.allowed_domains entry '{}' is not an exact hostname (no wildcard, IP address, port or path)",
                        duduclaw_core::truncate_chars(raw.trim(), 80)
                    )
                })?;
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
            }
            let max = duduclaw_core::types::COMPUTER_USE_MAX_ALLOWED_DOMAINS;
            if hosts.len() > max {
                return Err(format!("computer_use_config.allowed_domains holds at most {max} hosts"));
            }
            let n = hosts.len();
            sub.insert(
                "allowed_domains".into(),
                toml::Value::Array(hosts.into_iter().map(toml::Value::String).collect()),
            );
            changes.push(format!("capabilities.computer_use_config.allowed_domains = [{n} entries]"));
        }

        if let Some(v) = cfg.get("max_session_minutes").and_then(|v| v.as_u64()) {
            if v == 0 || v > 1440 {
                return Err("max_session_minutes must be 1-1440".into());
            }
            sub.insert("max_session_minutes".into(), toml::Value::Integer(v as i64));
            changes.push(format!(
                "capabilities.computer_use_config.max_session_minutes = {v}"
            ));
        }
        if let Some(v) = cfg.get("max_actions").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("max_actions must be 1-10000".into());
            }
            sub.insert("max_actions".into(), toml::Value::Integer(v as i64));
            changes.push(format!(
                "capabilities.computer_use_config.max_actions = {v}"
            ));
        }
        if let Some(v) = cfg.get("display_width").and_then(|v| v.as_u64()) {
            if !(320..=7680).contains(&v) {
                return Err("display_width must be 320-7680".into());
            }
            sub.insert("display_width".into(), toml::Value::Integer(v as i64));
            changes.push(format!(
                "capabilities.computer_use_config.display_width = {v}"
            ));
        }
        if let Some(v) = cfg.get("display_height").and_then(|v| v.as_u64()) {
            if !(240..=4320).contains(&v) {
                return Err("display_height must be 240-4320".into());
            }
            sub.insert("display_height".into(), toml::Value::Integer(v as i64));
            changes.push(format!(
                "capabilities.computer_use_config.display_height = {v}"
            ));
        }
        // 2026-10-08 keep-alive / takeover windows (P8). Strict: a present
        // key must be an integer in range, never silently skipped.
        for (key, lo, hi) in [
            (
                "keep_alive_minutes",
                0u64,
                duduclaw_core::types::COMPUTER_USE_MAX_KEEP_ALIVE_MINUTES as u64,
            ),
            (
                "takeover_idle_minutes",
                1u64,
                duduclaw_core::types::COMPUTER_USE_MAX_TAKEOVER_IDLE_MINUTES as u64,
            ),
        ] {
            if let Some(raw) = cfg.get(key) {
                let v = raw
                    .as_u64()
                    .filter(|v| (lo..=hi).contains(v))
                    .ok_or_else(|| format!("{key} must be an integer {lo}-{hi}"))?;
                sub.insert(key.into(), toml::Value::Integer(v as i64));
                changes.push(format!("capabilities.computer_use_config.{key} = {v}"));
            }
        }
        if let Some(v) = cfg.get("auto_confirm_trusted").and_then(|v| v.as_bool()) {
            sub.insert("auto_confirm_trusted".into(), toml::Value::Boolean(v));
            changes.push(format!(
                "capabilities.computer_use_config.auto_confirm_trusted = {v}"
            ));
        }
    }

    // ── native_sandbox (bool) — opt-in Seatbelt/Landlock OS confinement ──
    if let Some(v) = cap.get("native_sandbox").and_then(|v| v.as_bool()) {
        section.insert("native_sandbox".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.native_sandbox = {v}"));
    }

    // ── policy[] (Progent-style parameter-level tool policy) ──
    // Each entry: { tool, effect: allow|forbid|ask, when: [{arg, op, value}] }.
    // Non-empty `policy` flips the PolicyKernel into strict allowlist mode
    // (forbid > ask > allow; unmatched call denied) — so validate strictly and
    // fail-closed on any malformed rule rather than silently dropping it.
    if let Some(arr) = cap.get("policy").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
        for (i, rule) in arr.iter().enumerate() {
            let obj = rule
                .as_object()
                .ok_or_else(|| format!("capabilities.policy[{i}] must be an object"))?;

            let tool = obj
                .get("tool")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("capabilities.policy[{i}].tool must be a non-empty string")
                })?;

            let effect = match obj.get("effect").and_then(|v| v.as_str()) {
                Some("allow") => "allow",
                Some("forbid") => "forbid",
                Some("ask") => "ask",
                _ => {
                    return Err(format!(
                        "capabilities.policy[{i}].effect must be one of: allow, forbid, ask"
                    ));
                }
            };

            let mut rule_tbl = toml::map::Map::new();
            rule_tbl.insert("tool".into(), toml::Value::String(tool.into()));
            rule_tbl.insert("effect".into(), toml::Value::String(effect.into()));

            // Optional `when` conditions (logical AND). Absent/empty → matches
            // any arguments.
            if let Some(when_arr) = obj.get("when").and_then(|v| v.as_array()) {
                let mut conds: Vec<toml::Value> = Vec::with_capacity(when_arr.len());
                for (j, c) in when_arr.iter().enumerate() {
                    let cobj = c.as_object().ok_or_else(|| {
                        format!("capabilities.policy[{i}].when[{j}] must be an object")
                    })?;
                    let arg = cobj
                        .get("arg")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            format!(
                                "capabilities.policy[{i}].when[{j}].arg must be a non-empty string"
                            )
                        })?;
                    let op = match cobj.get("op").and_then(|v| v.as_str()) {
                        Some("equals") => "equals",
                        Some("contains") => "contains",
                        Some("starts_with") => "starts_with",
                        _ => {
                            return Err(format!(
                                "capabilities.policy[{i}].when[{j}].op must be one of: equals, contains, starts_with"
                            ));
                        }
                    };
                    // `value` is required but may legitimately be an empty string.
                    let value = cobj.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                        format!("capabilities.policy[{i}].when[{j}].value must be a string")
                    })?;
                    let mut cond_tbl = toml::map::Map::new();
                    cond_tbl.insert("arg".into(), toml::Value::String(arg.into()));
                    cond_tbl.insert("op".into(), toml::Value::String(op.into()));
                    cond_tbl.insert("value".into(), toml::Value::String(value.into()));
                    conds.push(toml::Value::Table(cond_tbl));
                }
                rule_tbl.insert("when".into(), toml::Value::Array(conds));
            }

            out.push(toml::Value::Table(rule_tbl));
        }
        let n = out.len();
        section.insert("policy".into(), toml::Value::Array(out));
        changes.push(format!("capabilities.policy = [{n} rules]"));
    }

    Ok(changes)
}
