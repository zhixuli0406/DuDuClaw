//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! IDR: identity.config_get/set round-trip + identity.resolve via the
//! wiki-cache provider (RFC-21 §1 dashboard surface). The Notion secret must
//! be write-only (masked on read); resolve must reuse the crate provider.
use super::*;

fn frame_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

fn frame_payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => Value::Null,
    }
}

fn frame_error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

// ── Pure-function tests ───────────────────────────────────────────────────

#[test]
fn response_defaults_when_section_absent() {
    let t = toml::Table::new();
    let resp = identity_table_to_response(&t);
    assert_eq!(resp.get("provider").unwrap().as_str(), Some("wiki_cache"));
    let notion = resp.get("notion").unwrap();
    assert_eq!(notion.get("api_key_set").unwrap().as_bool(), Some(false));
    assert_eq!(notion.get("api_key").unwrap().as_str(), Some(""));
    // field_map defaults present.
    let fm = notion.get("field_map").unwrap();
    assert_eq!(fm.get("name").unwrap().as_str(), Some("Name"));
    assert_eq!(
        fm.get("projects_kind").unwrap().as_str(),
        Some("multi_select")
    );
}

#[test]
fn response_masks_notion_secret() {
    let mut t = toml::Table::new();
    let mut notion = toml::map::Map::new();
    notion.insert(
        "api_key_enc".into(),
        toml::Value::String("ENCBLOB==".into()),
    );
    let mut idt = toml::map::Map::new();
    idt.insert("provider".into(), toml::Value::String("notion".into()));
    idt.insert("notion".into(), toml::Value::Table(notion));
    t.insert("identity".into(), toml::Value::Table(idt));

    let resp = identity_table_to_response(&t);
    let serialised = serde_json::to_string(&resp).unwrap();
    assert!(
        !serialised.contains("ENCBLOB"),
        "enc secret leaked: {serialised}"
    );
    let notion = resp.get("notion").unwrap();
    assert_eq!(notion.get("api_key_set").unwrap().as_bool(), Some(true));
    assert_eq!(
        notion.get("api_key").unwrap().as_str(),
        Some(SECRET_MASK_SET)
    );
}

#[test]
fn apply_rejects_invalid_provider() {
    let mut t = toml::Table::new();
    let mut changes = Vec::new();
    let err = apply_identity_to_table(&mut t, &json!({ "provider": "ldap" }), &mut changes)
        .unwrap_err();
    assert!(err.contains("Invalid provider"), "got: {err}");
}

#[test]
fn apply_rejects_invalid_projects_kind() {
    let mut t = toml::Table::new();
    let mut changes = Vec::new();
    let err = apply_identity_to_table(
        &mut t,
        &json!({ "notion": { "field_map": { "projects_kind": "bogus" } } }),
        &mut changes,
    )
    .unwrap_err();
    assert!(err.contains("projects_kind"), "got: {err}");
}

#[test]
fn apply_writes_provider_and_notion_fields() {
    let mut t = toml::Table::new();
    let mut changes = Vec::new();
    apply_identity_to_table(
        &mut t,
        &json!({
            "provider": "chained",
            "notion": {
                "database_id": "db_123",
                "refresh_seconds": 600,
                "field_map": {
                    "name": "Full Name",
                    "projects_kind": "relation",
                    "channel_props": { "discord": "DC", "slack": "SL" }
                }
            }
        }),
        &mut changes,
    )
    .unwrap();
    let idt = t.get("identity").unwrap().as_table().unwrap();
    assert_eq!(idt.get("provider").unwrap().as_str(), Some("chained"));
    let notion = idt.get("notion").unwrap().as_table().unwrap();
    assert_eq!(notion.get("database_id").unwrap().as_str(), Some("db_123"));
    let fm = notion.get("field_map").unwrap().as_table().unwrap();
    assert_eq!(fm.get("projects_kind").unwrap().as_str(), Some("relation"));
    let cp = fm.get("channel_props").unwrap().as_table().unwrap();
    assert_eq!(cp.get("slack").unwrap().as_str(), Some("SL"));
}

// ── Handler round-trip tests ──────────────────────────────────────────────

#[tokio::test]
async fn config_set_then_get_masks_secret_and_persists_fields() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let set = handler
        .handle_identity_config_set(json!({
            "provider": "notion",
            "notion": {
                "database_id": "db_abc",
                "api_key": "secret_topsecret",
                "field_map": { "name": "Person Name" }
            }
        }))
        .await;
    assert!(frame_ok(&set), "config_set should succeed: {set:?}");

    // Raw config.toml must NOT contain the cleartext secret.
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        !raw.contains("secret_topsecret"),
        "cleartext leaked to disk: {raw}"
    );
    assert!(
        raw.contains("api_key_enc"),
        "secret should persist encrypted"
    );

    let get = handler.handle_identity_config_get().await;
    assert!(frame_ok(&get));
    let p = frame_payload(&get);
    assert_eq!(p.get("provider").unwrap().as_str(), Some("notion"));
    let notion = p.get("notion").unwrap();
    assert_eq!(notion.get("database_id").unwrap().as_str(), Some("db_abc"));
    assert_eq!(notion.get("api_key_set").unwrap().as_bool(), Some(true));
    assert_eq!(
        notion.get("api_key").unwrap().as_str(),
        Some(SECRET_MASK_SET)
    );
}

#[tokio::test]
async fn config_set_masked_placeholder_does_not_overwrite_secret() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    handler
        .handle_identity_config_set(json!({
            "notion": { "api_key": "secret_original" }
        }))
        .await;
    // Re-save with the masked placeholder (what the dashboard echoes when the
    // operator leaves the field untouched) — the real secret must survive.
    handler
        .handle_identity_config_set(json!({
            "provider": "notion",
            "notion": { "api_key": SECRET_MASK_SET }
        }))
        .await;

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        !raw.contains(SECRET_MASK_SET),
        "placeholder must not be stored: {raw}"
    );
    assert!(raw.contains("api_key_enc"), "original secret must survive");
}

#[tokio::test]
async fn resolve_finds_person_via_wiki_cache() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Seed a wiki-cache identity record.
    let people = home
        .path()
        .join("shared")
        .join("wiki")
        .join("identity")
        .join("people");
    std::fs::create_dir_all(&people).unwrap();
    std::fs::write(
        people.join("ruby.md"),
        "---\n\
             person_id: person_2f9\n\
             display_name: Ruby Lin\n\
             roles: [customer-pm]\n\
             project_ids: [proj-alpha]\n\
             emails: [ruby@example.com]\n\
             channel_handles:\n  \
               email: \"ruby@example.com\"\n  \
               discord: \"1234567890\"\n\
             ---\n\nnotes\n",
    )
    .unwrap();

    let frame = handler
        .handle_identity_resolve(json!({
            "identifier": "ruby@example.com",
            "channel": "email"
        }))
        .await;
    assert!(frame_ok(&frame), "resolve should succeed: {frame:?}");
    let p = frame_payload(&frame);
    assert_eq!(p.get("found").unwrap().as_bool(), Some(true));
    assert_eq!(p.get("is_project_member").unwrap().as_bool(), Some(true));
    assert_eq!(p.get("provider").unwrap().as_str(), Some("wiki-cache"));
    let person = p.get("person").unwrap();
    assert_eq!(
        person.get("display_name").unwrap().as_str(),
        Some("Ruby Lin")
    );
}

#[tokio::test]
async fn resolve_miss_is_found_false_not_error() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_identity_resolve(json!({ "identifier": "nobody@example.com" }))
        .await;
    assert!(frame_ok(&frame), "a miss must not be an error: {frame:?}");
    let p = frame_payload(&frame);
    assert_eq!(p.get("found").unwrap().as_bool(), Some(false));
}

#[tokio::test]
async fn resolve_requires_identifier() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_identity_resolve(json!({})).await;
    assert!(!frame_ok(&frame));
    assert!(frame_error_text(&frame).contains("identifier"));
}
