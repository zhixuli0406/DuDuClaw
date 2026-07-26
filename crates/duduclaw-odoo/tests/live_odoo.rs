//! Live-instance JSON-RPC syntax verification for `OdooConnector`.
//!
//! These tests hit a *real* Odoo instance (never mocked) to verify that
//! `introspect_schema` / `schema_fields` / `partner_search` speak correct
//! Odoo ORM JSON-RPC — field names on `ir.model` / `ir.model.fields`, and the
//! Polish-notation `domain` list for OR queries. They are `#[ignore]`d by
//! default (no CI dependency on a live Odoo) and driven entirely by env vars:
//!
//! ```text
//! ODOO_LIVE_URL=http://127.0.0.1:8069
//! ODOO_LIVE_DB=livetest
//! ODOO_LIVE_USER=admin
//! ODOO_LIVE_PASSWORD=admin
//! cargo test -p duduclaw-odoo --test live_odoo -- --ignored --nocapture
//! ```

use duduclaw_odoo::{OdooConfig, OdooConnector, PARTNER_SEARCH_FIELDS};

fn env_or_skip(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

async fn connect() -> OdooConnector {
    let url = env_or_skip("ODOO_LIVE_URL").expect("ODOO_LIVE_URL not set");
    let db = env_or_skip("ODOO_LIVE_DB").expect("ODOO_LIVE_DB not set");
    let user = env_or_skip("ODOO_LIVE_USER").expect("ODOO_LIVE_USER not set");
    let password = env_or_skip("ODOO_LIVE_PASSWORD").expect("ODOO_LIVE_PASSWORD not set");

    let config = OdooConfig {
        url,
        db,
        username: user,
        ..Default::default()
    };

    OdooConnector::connect(&config, &password)
        .await
        .expect("connect/authenticate against live Odoo failed")
}

#[tokio::test]
#[ignore]
async fn live_connect_and_authenticate() {
    let conn = connect().await;
    assert!(conn.uid.is_some(), "expected a uid after authentication");
    println!("[live] connected, uid={:?}", conn.uid);
}

#[tokio::test]
#[ignore]
async fn live_introspect_schema() {
    let conn = connect().await;
    let report = conn
        .introspect_schema(50)
        .await
        .expect("introspect_schema RPC failed");

    println!(
        "[live] introspect_schema: total_models={} truncated={} kept={}",
        report.total_models,
        report.truncated,
        report.models.len()
    );
    assert!(!report.models.is_empty(), "expected at least one model");

    let partner = report
        .models
        .iter()
        .find(|m| m.model == "res.partner")
        .expect("res.partner should be discoverable via ir.model introspection");

    println!(
        "[live] res.partner: name={} field_count={}",
        partner.name, partner.field_count
    );
    assert!(!partner.fields.is_empty(), "res.partner should have fields");

    // Field metadata sanity: name/ttype/label populated for a well-known field.
    let name_field = partner
        .fields
        .iter()
        .find(|f| f.name == "name")
        .expect("res.partner.name field should be present");
    println!(
        "[live] res.partner.name field: ttype={} label={:?}",
        name_field.ttype, name_field.label
    );
    assert_eq!(name_field.ttype, "char");
    assert!(
        !name_field.label.is_empty(),
        "field_description (label) must not be empty — verifies the ir.model.fields column name is correct"
    );
}

#[tokio::test]
#[ignore]
async fn live_schema_fields_res_partner() {
    let conn = connect().await;
    let fields = conn
        .schema_fields("res.partner", 200)
        .await
        .expect("schema_fields (fields_get) RPC failed");

    println!("[live] schema_fields(res.partner): {} fields", fields.len());
    assert!(!fields.is_empty());

    let email_field = fields
        .iter()
        .find(|f| f.name == "email")
        .expect("res.partner.email field should be present via fields_get");
    println!(
        "[live] email field: ttype={} label={:?} required={}",
        email_field.ttype, email_field.label, email_field.required
    );
    assert_eq!(email_field.ttype, "char");
}

#[tokio::test]
#[ignore]
async fn live_partner_search_azure_demo() {
    let conn = connect().await;
    let result = conn
        .partner_search("Azure", 10)
        .await
        .expect("partner_search RPC failed");

    let arr = result.as_array().expect("partner_search should return an array");
    println!(
        "[live] partner_search('Azure'): {} row(s): {}",
        arr.len(),
        serde_json::to_string(&result).unwrap_or_default()
    );
    assert!(
        !arr.is_empty(),
        "expected at least one demo partner matching 'Azure' (e.g. Azure Interior)"
    );

    // Whitelist enforcement: every returned row must only contain
    // PARTNER_SEARCH_FIELDS (+ Odoo's implicit id/display_name is fine since
    // PARTNER_SEARCH_FIELDS includes "id").
    for row in arr {
        let obj = row.as_object().expect("row should be a JSON object");
        for key in obj.keys() {
            assert!(
                PARTNER_SEARCH_FIELDS.contains(&key.as_str()),
                "partner_search leaked a non-whitelisted field: {key}"
            );
        }
    }
}
