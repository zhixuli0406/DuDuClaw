//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// WP-H1 (credentials doctrine 2026-08): `mask_sensitive_fields` must recurse
/// into arrays and arrays-of-tables, not just nested tables — otherwise a
/// plaintext `oauth_token` inside `[[accounts]]` (the standard shape for
/// multi-account config) survives the mask and is readable verbatim via the
/// `system.config` dashboard RPC.
use super::*;

fn parse(toml_str: &str) -> toml::Table {
    toml_str.parse::<toml::Table>().expect("valid toml fixture")
}

#[test]
fn masks_oauth_token_inside_array_of_tables() {
    let mut table = parse(
        r#"
            [[accounts]]
            id = "acct-1"
            oauth_token = "sk-live-plaintext-secret"
            label = "Primary"

            [[accounts]]
            id = "acct-2"
            oauth_token = "sk-live-another-secret"
            "#,
    );
    MethodHandler::mask_sensitive_fields(&mut table);

    let accounts = table.get("accounts").and_then(|v| v.as_array()).unwrap();
    assert_eq!(accounts.len(), 2);
    for acc in accounts {
        let t = acc.as_table().unwrap();
        assert_eq!(
            t.get("oauth_token").and_then(|v| v.as_str()),
            Some("********"),
            "oauth_token inside [[accounts]] must be masked: {t:?}"
        );
        assert!(
            t.get("id").and_then(|v| v.as_str()).is_some(),
            "non-sensitive fields must survive: {t:?}"
        );
    }
    assert_eq!(
        accounts[0]
            .as_table()
            .unwrap()
            .get("label")
            .and_then(|v| v.as_str()),
        Some("Primary"),
        "non-sensitive fields must not be masked"
    );
}

#[test]
fn masks_sensitive_key_nested_two_tables_deep() {
    let mut table = parse(
        r#"
            [channels.telegram]
            bot_token = "123456:ABC-plaintext"
            chat_id = "789"
            "#,
    );
    MethodHandler::mask_sensitive_fields(&mut table);
    let telegram = table
        .get("channels")
        .and_then(|v| v.as_table())
        .and_then(|c| c.get("telegram"))
        .and_then(|v| v.as_table())
        .unwrap();
    assert_eq!(
        telegram.get("bot_token").and_then(|v| v.as_str()),
        Some("********")
    );
    assert_eq!(
        telegram.get("chat_id").and_then(|v| v.as_str()),
        Some("789")
    );
}

#[test]
fn masks_sensitive_key_holding_array_of_strings() {
    let mut table = parse(
        r#"
            api_keys = ["key-one", "key-two"]
            names = ["alice", "bob"]
            "#,
    );
    MethodHandler::mask_sensitive_fields(&mut table);
    let keys = table.get("api_keys").and_then(|v| v.as_array()).unwrap();
    for k in keys {
        assert_eq!(k.as_str(), Some("********"));
    }
    let names = table.get("names").and_then(|v| v.as_array()).unwrap();
    assert_eq!(names[0].as_str(), Some("alice"));
    assert_eq!(names[1].as_str(), Some("bob"));
}

#[test]
fn leaves_empty_sensitive_string_unmasked() {
    // Matches pre-existing behavior: an unset (empty string) secret is
    // left as-is rather than replaced with "********", which would
    // falsely imply a secret is configured.
    let mut table = parse(r#"password = """#);
    MethodHandler::mask_sensitive_fields(&mut table);
    assert_eq!(table.get("password").and_then(|v| v.as_str()), Some(""));
}

#[test]
fn masks_table_nested_inside_array_of_tables_entry() {
    let mut table = parse(
        r#"
            [[agents]]
            id = "agent-1"

            [agents.odoo]
            api_key = "plaintext-odoo-key"
            "#,
    );
    MethodHandler::mask_sensitive_fields(&mut table);
    let agents = table.get("agents").and_then(|v| v.as_array()).unwrap();
    let odoo = agents[0]
        .as_table()
        .unwrap()
        .get("odoo")
        .and_then(|v| v.as_table())
        .unwrap();
    assert_eq!(
        odoo.get("api_key").and_then(|v| v.as_str()),
        Some("********")
    );
}

/// The second `system.config` leak from the same design section: the MCP
/// API key is the **table key name**, so a value-only mask never touched
/// it and `[mcp_keys."ddc_prod_…"]` rendered verbatim — and that key is the
/// gateway's own admin-scope credential.
#[test]
fn masks_mcp_key_table_names() {
    let mut table = parse(
        r#"
            [mcp_keys.ddc_prod_a1b2c3d4e5f6]
            scopes = ["admin"]
            created = "2026-08-15"
            "#,
    );
    MethodHandler::mask_sensitive_fields(&mut table);
    MethodHandler::mask_keyed_secret_tables(&mut table);

    let rendered = toml::to_string_pretty(&table).unwrap();
    assert!(
        !rendered.contains("ddc_prod_a1b2c3d4e5f6"),
        "full MCP key must not survive rendering: {rendered}"
    );
    let keys = table.get("mcp_keys").and_then(|v| v.as_table()).unwrap();
    assert_eq!(keys.len(), 1, "the entry itself must survive: {keys:?}");
    // Metadata under the (now masked) key is untouched.
    let entry = keys.values().next().and_then(|v| v.as_table()).unwrap();
    assert_eq!(
        entry.get("created").and_then(|v| v.as_str()),
        Some("2026-08-15")
    );
}

/// Two keys that mask to the same display form must not collapse into one
/// row — a silently vanishing credential is worse than a duplicate label.
#[test]
fn masked_mcp_key_collisions_keep_every_entry() {
    let mut table = parse(
        r#"
            [mcp_keys.ddc_prod_aaaabbbbcccc]
            scopes = ["admin"]

            [mcp_keys.ddc_prod_aaaadddddddd]
            scopes = ["read"]
            "#,
    );
    MethodHandler::mask_keyed_secret_tables(&mut table);
    let keys = table.get("mcp_keys").and_then(|v| v.as_table()).unwrap();
    assert_eq!(keys.len(), 2, "both entries must survive: {keys:?}");
    let rendered = toml::to_string_pretty(&table).unwrap();
    assert!(!rendered.contains("aaaabbbbcccc") && !rendered.contains("aaaadddddddd"));
}

/// No `[mcp_keys]` section is a no-op, not a panic.
#[test]
fn mask_keyed_secret_tables_is_a_no_op_without_the_section() {
    let mut table = parse("[channels]\ntelegram_bot_token = \"x\"\n");
    let before = table.clone();
    MethodHandler::mask_keyed_secret_tables(&mut table);
    assert_eq!(table, before);
}
