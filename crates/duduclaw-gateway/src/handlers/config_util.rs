//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── W2-2 (E1/E2): channel behavior/access settings ⇄ JSON ───────────────
//
// `ChannelSettingsManager::get_all` returns raw `(key, value)` string pairs
// (its on-disk encoding); the dashboard form needs typed JSON. These two
// converters are the read-side counterpart of `json_value_to_setting_string`
// below and share its key list, so a key present in one and not the other
// would be caught immediately by the round-trip unit tests.

/// Render a channel's `CONFIG_KEYS` rows as typed JSON, applying the same
/// defaults the live reply path uses when a key was never set (`mention_only`
/// / `auto_thread` default `false`, `response_mode` defaults `"auto"` —
/// mirrors `channel_reply.rs`/`discord.rs` `get_bool`/`get_with_fallback`
/// call sites). `thread_archive_minutes` and `agent_override` have no forced
/// default (`null` / `""`) — "unset" is a meaningful, distinct state for
/// both (fall back to per-message default; no override respectively).
pub(crate) fn config_settings_to_json(all: &[(String, String)]) -> Value {
    let get = |k: &str| all.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let json_array = |k: &str| -> Value {
        get(k)
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default()
            .into()
    };
    json!({
        "mention_only": get(crate::channel_settings::keys::MENTION_ONLY).as_deref() == Some("true"),
        "auto_thread": get(crate::channel_settings::keys::AUTO_THREAD).as_deref() == Some("true"),
        "allowed_channels": json_array(crate::channel_settings::keys::ALLOWED_CHANNELS),
        "allowed_guilds": json_array(crate::channel_settings::keys::ALLOWED_GUILDS),
        "agent_override": get(crate::channel_settings::keys::AGENT_OVERRIDE).unwrap_or_default(),
        "response_mode": get(crate::channel_settings::keys::RESPONSE_MODE).unwrap_or_else(|| "auto".to_string()),
        "thread_archive_minutes": get(crate::channel_settings::keys::THREAD_ARCHIVE_MINUTES),
    })
}

/// Render a channel's `DASHBOARD_ACCESS_KEYS` rows as typed JSON. All four
/// default to the fully-open state (`require_pairing: false`, empty lists) —
/// matching `check_user_access_gate`'s documented "defaults are fully open"
/// contract in `channel_reply.rs`, so a never-configured channel reads the
/// same way the dashboard is about to render it.
pub(crate) fn access_settings_to_json(all: &[(String, String)]) -> Value {
    let get = |k: &str| all.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let json_array = |k: &str| -> Value {
        get(k)
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default()
            .into()
    };
    json!({
        "require_pairing": get(crate::channel_settings::keys::REQUIRE_PAIRING).as_deref() == Some("true"),
        "allowed_users": json_array(crate::channel_settings::keys::ALLOWED_USERS),
        "blocked_users": json_array(crate::channel_settings::keys::BLOCKED_USERS),
        "admin_users": json_array(crate::channel_settings::keys::ADMIN_USERS),
    })
}

/// Convert one dashboard-form JSON value to the string encoding
/// `ChannelSettingsManager` stores (and `validate_setting_value` checks).
/// The write-side counterpart of `config_settings_to_json`/
/// `access_settings_to_json` above.
pub(crate) fn json_value_to_setting_string(key: &str, value: &Value) -> std::result::Result<String, String> {
    use crate::channel_settings::keys;
    match key {
        k if k == keys::MENTION_ONLY || k == keys::AUTO_THREAD || k == keys::REQUIRE_PAIRING => {
            value
                .as_bool()
                .map(|b| b.to_string())
                .ok_or_else(|| format!("{key} must be a boolean"))
        }
        k if k == keys::ALLOWED_CHANNELS
            || k == keys::ALLOWED_GUILDS
            || k == keys::ALLOWED_USERS
            || k == keys::BLOCKED_USERS
            || k == keys::ADMIN_USERS =>
        {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("{key} must be an array of strings"))?;
            let strs: std::result::Result<Vec<String>, String> = arr
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{key} must be an array of strings"))
                })
                .collect();
            serde_json::to_string(&strs?).map_err(|e| e.to_string())
        }
        k if k == keys::THREAD_ARCHIVE_MINUTES => {
            if let Some(n) = value.as_u64() {
                Ok(n.to_string())
            } else if let Some(s) = value.as_str() {
                Ok(s.to_string())
            } else {
                Err(format!("{key} must be a number or a numeric string"))
            }
        }
        _ => value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{key} must be a string")),
    }
}

/// Redact URLs and credential-like tokens from a free-form error string before
/// it is forwarded to a client (M19).
///
/// Operates per whitespace-separated token so surrounding prose is preserved:
/// - Tokens containing a scheme (`://`) have any `user:pass@` userinfo and any
///   `?query` string stripped, leaving `scheme://host[:port]/path`.
/// - `key=value` pairs whose key looks sensitive (api_key / token / password /
///   secret / pwd / auth) have their value replaced with `[REDACTED]`.
pub(crate) fn scrub_secrets_from_text(raw: &str) -> String {
    fn is_sensitive_key(key: &str) -> bool {
        let k = key.to_ascii_lowercase();
        [
            "api_key", "apikey", "token", "password", "passwd", "pwd", "secret", "auth", "key",
        ]
        .iter()
        .any(|s| k.contains(s))
    }

    fn scrub_token(token: &str) -> String {
        // 1. URL: strip userinfo + query string, keep scheme://host/path.
        if let Some(scheme_idx) = token.find("://") {
            let scheme = &token[..scheme_idx];
            let rest = &token[scheme_idx + 3..];
            // Drop everything from the first query/fragment marker onward.
            let rest = rest.split(['?', '#']).next().unwrap_or("");
            // Drop userinfo (anything up to and including '@' in the authority).
            // The authority ends at the first '/'.
            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            let host = authority.rsplit('@').next().unwrap_or(authority);
            return format!("{scheme}://{host}{path}");
        }
        // 2. key=value: redact sensitive values.
        if let Some(eq) = token.find('=') {
            let (key, _val) = token.split_at(eq);
            if is_sensitive_key(key) {
                return format!("{key}=[REDACTED]");
            }
        }
        token.to_string()
    }

    // WP12: this scrubber keeps `scheme://host{path}` intact, so a credential
    // carried in the PATH (Telegram's `/bot<token>/getMe`) survived it. Run the
    // shape-driven redactor first, then the existing URL/kv rules.
    let raw = crate::secret_redact::redact_secrets(raw).into_owned();
    raw.split_whitespace()
        .map(scrub_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hours elapsed since the start of the current UTC month — the look-back
/// window CostTelemetry uses to compute "this month" spend. Always ≥ 1 so the
/// telemetry query never receives a zero window.
pub(crate) fn hours_since_month_start() -> u64 {
    let now = Utc::now();
    let month_start = now
        .date_naive()
        .with_day(1)
        .unwrap_or(now.date_naive())
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default();
    let month_start_utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(month_start, Utc);
    (now - month_start_utc).num_hours().max(1) as u64
}

/// Validate wiki page path: relative, .md suffix, no traversal, no NUL.
/// Mirrors `WikiStore::validate_page_path` for use at the WS RPC boundary
/// (review H2/M6 — page_path enters audit log + downstream file ops).
pub(crate) fn is_safe_wiki_page_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains("..")
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\0')
        && !path.contains("%2e")
        && !path.contains("%2E")
        && !path.contains("%2f")
        && !path.contains("%2F")
        && path.ends_with(".md")
}
