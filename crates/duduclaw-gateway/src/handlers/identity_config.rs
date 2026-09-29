//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── IDR: [identity] in config.toml (RFC-21 §1 dashboard surface) ──────────────
// The API-level twin of the `identity_resolve` MCP tool: lets the operator pick
// which `duduclaw_identity` provider is active and test-resolve an identifier
// from the dashboard. Secret handling mirrors the [openai_compat] pattern above.

/// Valid `identity.provider` selectors.
pub(crate) const IDENTITY_PROVIDERS: [&str; 3] = ["wiki_cache", "notion", "chained"];

/// Default Notion field_map — mirrors `NotionFieldMap::default()` in
/// `duduclaw-identity` so the dashboard shows the same defaults the resolver
/// uses when the operator hasn't overridden them.
pub(crate) fn default_notion_field_map() -> Value {
    json!({
        "name": "Name",
        "roles": "Roles",
        "projects": "Projects",
        "projects_kind": "multi_select",
        "emails": "Email",
        "channel_props": {
            "discord": "Discord ID",
            "line": "Line ID",
            "telegram": "Telegram ID",
            "email": "Email"
        }
    })
}

/// Build the `identity.config_get` response from config.toml, MASKING the Notion
/// api key (never returns cleartext or the `_enc` blob). Missing values fall back
/// to explicit defaults so the caller never sees `null`.
pub(crate) fn identity_table_to_response(table: &toml::Table) -> Value {
    let idt = table.get("identity").and_then(|v| v.as_table());
    let provider = idt
        .and_then(|t| t.get("provider"))
        .and_then(|v| v.as_str())
        .filter(|s| IDENTITY_PROVIDERS.contains(s))
        .unwrap_or("wiki_cache")
        .to_string();

    let notion_tbl = idt.and_then(|t| t.get("notion")).and_then(|v| v.as_table());
    let database_id = notion_tbl
        .and_then(|t| t.get("database_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let refresh_seconds = notion_tbl
        .and_then(|t| t.get("refresh_seconds"))
        .and_then(|v| v.as_integer())
        .unwrap_or(900);
    let has_secret = notion_tbl
        .and_then(|t| t.get("api_key_enc").or_else(|| t.get("api_key")))
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    // field_map: start from defaults, overlay any operator overrides.
    let mut field_map = default_notion_field_map();
    if let Some(fm) = notion_tbl
        .and_then(|t| t.get("field_map"))
        .and_then(|v| v.as_table())
    {
        if let Some(obj) = field_map.as_object_mut() {
            for key in ["name", "roles", "projects", "projects_kind", "emails"] {
                if let Some(s) = fm.get(key).and_then(|v| v.as_str()) {
                    obj.insert(key.into(), json!(s));
                }
            }
            if let Some(cp) = fm.get("channel_props").and_then(|v| v.as_table()) {
                let mut props = serde_json::Map::new();
                for (k, v) in cp {
                    if let Some(s) = v.as_str() {
                        props.insert(k.clone(), json!(s));
                    }
                }
                if !props.is_empty() {
                    obj.insert("channel_props".into(), Value::Object(props));
                }
            }
        }
    }

    json!({
        "provider": provider,
        "notion": {
            "database_id": database_id,
            "refresh_seconds": refresh_seconds,
            "api_key_set": has_secret,
            "api_key": if has_secret { SECRET_MASK_SET } else { "" },
            "field_map": field_map,
        }
    })
}

/// Apply an `identity.config_set` params object onto config.toml's `[identity]`
/// table (non-secret fields only — the Notion api key is encrypted by the async
/// handler). Returns the human-readable change list. Validates the provider enum
/// and `projects_kind`. Fails closed on malformed sections.
pub(crate) fn apply_identity_to_table(
    table: &mut toml::Table,
    params: &Value,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    let obj = params.as_object().ok_or("params must be an object")?;

    let idt = table
        .entry("identity")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("Invalid [identity] section")?;

    if let Some(p) = obj.get("provider").and_then(|v| v.as_str()) {
        if !IDENTITY_PROVIDERS.contains(&p) {
            return Err(format!(
                "Invalid provider '{p}'; must be one of wiki_cache | notion | chained"
            ));
        }
        idt.insert("provider".into(), toml::Value::String(p.to_string()));
        changes.push(format!("identity.provider = {p}"));
    }

    if let Some(notion) = obj.get("notion").and_then(|v| v.as_object()) {
        let nt = idt
            .entry("notion")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or("Invalid [identity.notion] section")?;

        if let Some(db) = notion.get("database_id").and_then(|v| v.as_str()) {
            nt.insert(
                "database_id".into(),
                toml::Value::String(db.trim().to_string()),
            );
            changes.push("identity.notion.database_id updated".to_string());
        }
        if let Some(rs) = notion.get("refresh_seconds").and_then(|v| v.as_i64()) {
            if rs < 0 {
                return Err("refresh_seconds must be >= 0".to_string());
            }
            nt.insert("refresh_seconds".into(), toml::Value::Integer(rs));
            changes.push(format!("identity.notion.refresh_seconds = {rs}"));
        }

        if let Some(fm) = notion.get("field_map").and_then(|v| v.as_object()) {
            let fmt = nt
                .entry("field_map")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .ok_or("Invalid [identity.notion.field_map] section")?;
            for key in ["name", "roles", "projects", "emails"] {
                if let Some(s) = fm.get(key).and_then(|v| v.as_str()) {
                    fmt.insert(key.into(), toml::Value::String(s.to_string()));
                    changes.push(format!("identity.notion.field_map.{key} updated"));
                }
            }
            if let Some(pk) = fm.get("projects_kind").and_then(|v| v.as_str()) {
                if pk != "multi_select" && pk != "relation" {
                    return Err("projects_kind must be multi_select | relation".to_string());
                }
                fmt.insert("projects_kind".into(), toml::Value::String(pk.to_string()));
                changes.push(format!("identity.notion.field_map.projects_kind = {pk}"));
            }
            if let Some(cp) = fm.get("channel_props").and_then(|v| v.as_object()) {
                // Replace channel_props wholesale (operator sends the full set).
                let mut new_props = toml::map::Map::new();
                for (k, v) in cp {
                    if let Some(s) = v.as_str() {
                        new_props.insert(k.clone(), toml::Value::String(s.to_string()));
                    }
                }
                fmt.insert("channel_props".into(), toml::Value::Table(new_props));
                changes.push("identity.notion.field_map.channel_props updated".to_string());
            }
        }
    }

    Ok(())
}
