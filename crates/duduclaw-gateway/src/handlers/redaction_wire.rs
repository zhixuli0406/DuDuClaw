//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

pub(crate) fn redaction_table_to_response(table: &toml::Table) -> Value {
    let red = table.get("redaction").and_then(|v| v.as_table());
    let enabled = red
        .and_then(|r| r.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let vault_ttl = red
        .and_then(|r| r.get("vault_ttl_hours"))
        .and_then(|v| v.as_integer())
        .unwrap_or(168);
    let purge = red
        .and_then(|r| r.get("purge_after_expire_days"))
        .and_then(|v| v.as_integer())
        .unwrap_or(30);
    let profiles: Vec<String> = red
        .and_then(|r| r.get("profiles"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let sources = red
        .and_then(|r| r.get("sources"))
        .and_then(|v| v.as_table());
    // Sources round-trip both TOML forms (bare mode string / detail table);
    // the wire response is always the detail-object form.
    let source_setting = |key: &str, default: &str| -> Value {
        let toml_cats = |t: &toml::Table, field: &str| -> Vec<String> {
            t.get(field)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        match sources.and_then(|s| s.get(key)) {
            Some(toml::Value::String(mode)) => json!({
                "mode": mode,
                "only_categories": [],
                "exclude_categories": [],
            }),
            Some(toml::Value::Table(t)) => json!({
                "mode": t.get("mode").and_then(|v| v.as_str()).unwrap_or(default),
                "only_categories": toml_cats(t, "only_categories"),
                "exclude_categories": toml_cats(t, "exclude_categories"),
            }),
            _ => json!({
                "mode": default,
                "only_categories": [],
                "exclude_categories": [],
            }),
        }
    };

    let egress = red
        .and_then(|r| r.get("tool_egress"))
        .and_then(|v| v.as_table());
    let mut egress_out = serde_json::Map::new();
    if let Some(eg) = egress {
        for (tool, rule) in eg {
            let rule_t = rule.as_table();
            let restore = rule_t
                .and_then(|t| t.get("restore_args"))
                .and_then(|v| v.as_str())
                .unwrap_or("deny");
            let audit = rule_t
                .and_then(|t| t.get("audit_reveal"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            egress_out.insert(
                tool.clone(),
                json!({ "restore_args": restore, "audit_reveal": audit }),
            );
        }
    }

    json!({
        "enabled": enabled,
        "vault_ttl_hours": vault_ttl,
        "purge_after_expire_days": purge,
        "profiles": profiles,
        "sources": {
            "user_input": source_setting("user_input", "off"),
            "tool_results": source_setting("tool_results", "on"),
            "system_prompt": source_setting("system_prompt", "selective"),
            "sub_agent": source_setting("sub_agent", "inherit"),
            "cron_context": source_setting("cron_context", "on"),
        },
        "tool_egress": Value::Object(egress_out),
    })
}
