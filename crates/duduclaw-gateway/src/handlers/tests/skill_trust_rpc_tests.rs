use super::*;
use duduclaw_core::publishers::{TrustStore, OFFICIAL_PUBLISHER_KEY, TRUST_STORE_FILE};
use crate::approval::{ApprovalBroker, ApprovalId};
use crate::skill_trust::minisign_key_id;

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    admin: UserContext,
    member: UserContext,
}
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "# keep this comment\n[approval]\nskill_ttl_days=9\n").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let admin = db.create_user(&format!("admin-{}@test.invalid", uuid::Uuid::new_v4()), "Admin", "test-password", UserRole::Admin).unwrap();
    let member = db.create_user("member@test.invalid", "Member", "test-password", UserRole::Employee).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"skill-trust-rpc-test-secret-32-bytes"))).await;
    Fixture { admin: db.live_user_context(&admin.id).unwrap().unwrap(),
        member: db.live_user_context(&member.id).unwrap().unwrap(), home, handler }
}
async fn browser(f: &Fixture, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
    crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), uuid::Uuid::new_v4().to_string())),
        f.handler.handle_conn(method, params, ctx,
            crate::power_local::RpcConnInfo::from_ws("127.0.0.1:12345".parse().unwrap(), false))).await
}
fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected success: {other:?}"),
    }
}
fn refused(frame: WsFrame) {
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "expected refusal: {frame:?}");
}
fn args() -> Value {
    json!({"publisher": "in-house", "public_key": OFFICIAL_PUBLISHER_KEY,
        "acknowledge_risk": true, "password": "test-password"})
}
fn key_id() -> String { minisign_key_id(OFFICIAL_PUBLISHER_KEY).unwrap() }
fn policy() -> String { format!("policy:skill_trust:in-house:{}", key_id()) }
fn config(f: &Fixture) -> String { std::fs::read_to_string(f.home.path().join("config.toml")).unwrap() }
fn audit(f: &Fixture, event: &str) -> Vec<Value> {
    std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap().lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()).filter(|row| row["event_type"] == event).collect()
}
async fn approve(f: &Fixture, slug: &str, entries: Value, decided_by: &str) -> ApprovalId {
    let broker = ApprovalBroker::open(f.home.path()).unwrap();
    let id = broker.request("alice", crate::expert_skills::SKILLS_ACTION_KIND, "skills",
        json!({"slug": slug, "entries": entries, "auto_approval_candidate": true}), 3600).await.unwrap();
    if decided_by.split(':').take(2).eq(["policy", "skill_trust"]) {
        broker.decide_by_skill_trust_policy(&id, decided_by).await.unwrap();
    } else {
        broker.decide(&id, true, decided_by).await.unwrap();
    }
    id
}
fn promoted(f: &Fixture, name: &str) -> (PathBuf, String) {
    let path = f.home.path().join("agents/alice/SKILLS").join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("SKILL.md"), format!("# {name}\nApproved skill content\n")).unwrap();
    let digest = crate::expert_skills::tree_digest(&path).unwrap();
    (path, digest)
}

#[tokio::test]
async fn pin_requires_admin_and_reauth() {
    let f = fixture().await;
    let before = config(&f);
    for method in ["skill_trust.get", "skill_trust.pin", "skill_trust.unpin"] {
        refused(browser(&f, &f.member, method, args()).await);
    }
    refused(f.handler.handle("skill_trust.pin", args(), &f.admin).await);
    refused(browser(&f, &UserContext::admin_fallback(), "skill_trust.pin", args()).await);
    for acknowledgement in [Value::Null, json!(false), json!("true"), json!(1)] {
        let mut params = args();
        params["acknowledge_risk"] = acknowledgement;
        refused(browser(&f, &f.admin, "skill_trust.pin", params).await);
    }
    for password in ["", "wrong"] {
        let mut params = args();
        params["password"] = json!(password);
        params["code"] = json!("wrong");
        params["added_by"] = json!("forged");
        refused(browser(&f, &f.admin, "skill_trust.pin", params).await);
    }
    assert_eq!(config(&f), before);
    assert!(!f.home.path().join(TRUST_STORE_FILE).exists());
    let mut params = args();
    params["added_by"] = json!("forged");
    let entry = payload(browser(&f, &f.admin, "skill_trust.pin", params).await);
    assert_eq!(entry["added_by"], f.admin.user_id);
    assert_eq!(entry["key_id"], key_id());
    assert!(chrono::DateTime::parse_from_rfc3339(entry["added_at"].as_str().unwrap()).is_ok());
    assert_eq!(TrustStore::load(f.home.path()).unwrap().keys("in-house"), vec![OFFICIAL_PUBLISHER_KEY]);
    assert!(config(&f).contains("# keep this comment"));
    let rows = audit(&f, "skill_trust_pinned");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["agent_id"], f.admin.user_id);
    assert_eq!(rows[0]["details"]["publisher"], "in-house");
    assert_eq!(rows[0]["details"]["trust_store_changed"], true);
    let db = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM skill_trust_pin_proofs", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let mut replay = args();
    replay.as_object_mut().unwrap().remove("password");
    replay["proof"] = json!("client-supplied-proof");
    replay["publisher"] = json!("second-source");
    let before = config(&f);
    let trust_before = std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap();
    refused(browser(&f, &f.admin, "skill_trust.pin", replay).await);
    assert_eq!(config(&f), before);
    assert_eq!(std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap(), trust_before);
}

#[tokio::test]
async fn pin_refuses_builtin_and_invalid_key_and_is_idempotent() {
    let f = fixture().await;
    let before = config(&f);
    let mut builtin = args(); builtin["publisher"] = json!("duduclaw");
    refused(browser(&f, &f.admin, "skill_trust.pin", builtin).await);
    let mut invalid = args(); invalid["public_key"] = json!("broken");
    refused(browser(&f, &f.admin, "skill_trust.pin", invalid).await);
    assert_eq!(config(&f), before);
    assert!(!f.home.path().join(TRUST_STORE_FILE).exists());
    let first = payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let second = payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    assert_eq!(first, second);
    let table: toml::Table = config(&f).parse().unwrap();
    assert_eq!(table["skill_trust"]["auto_approve"].as_array().unwrap().len(), 1);
    assert_eq!(TrustStore::load(f.home.path()).unwrap().keys("in-house").len(), 1);
    assert_eq!(audit(&f, "skill_trust_pinned")[1]["details"]["trust_store_changed"], false);
}

#[tokio::test]
async fn pin_requires_current_mfa_and_server_session_and_socket_peer() {
    let f = fixture().await;
    let before = config(&f);
    let raw = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'unusable-test-secret',0)", [&f.admin.user_id]).unwrap();
    refused(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let result = crate::google_personal::DASHBOARD_SESSION.scope(Some((f.admin.user_id.clone(), uuid::Uuid::new_v4().to_string())),
        f.handler.handle_conn("skill_trust.pin", json!({"publisher":"in-house", "public_key":OFFICIAL_PUBLISHER_KEY,
            "acknowledge_risk":true, "password":"test-password", "peer":"127.0.0.1:12345"}), &f.admin,
            crate::power_local::RpcConnInfo::internal())).await;
    refused(result);
    assert_eq!(config(&f), before);
    assert!(!f.home.path().join(TRUST_STORE_FILE).exists());
}

#[tokio::test]
async fn raw_editor_refuses_skill_trust() {
    let f = fixture().await;
    payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let before = config(&f);
    let got = payload(f.handler.handle("config.raw.get", json!({"file":"config"}), &f.admin).await);
    assert!(!got["content"].as_str().unwrap().contains("skill_trust"));
    assert!(!got["content"].as_str().unwrap().contains(OFFICIAL_PUBLISHER_KEY));
    assert!(got["hidden_sections"].as_array().unwrap().contains(&json!("skill_trust")));
    for section in ["[skill_trust]\nauto_approve=[]\n", "skill_trust = { auto_approve = [] }\n", "[[skill_trust.auto_approve]]\npublisher='in-house'\n"] {
        refused(f.handler.handle("config.raw.set", json!({"file":"config", "content": section, "base_hash":got["hash"]}), &f.admin).await);
        assert_eq!(config(&f), before);
    }
    let restored = super::super::config_raw_rpc::restore_raw_toml(got["content"].as_str().unwrap(), &before).unwrap();
    assert_eq!(restored.parse::<toml::Table>().unwrap()["skill_trust"], before.parse::<toml::Table>().unwrap()["skill_trust"]);
    for params in [json!({"skill_trust":{"auto_approve":[]}}), json!({"skill_trust.auto_approve":[]}), json!({"skill_trust":null})] {
        refused(f.handler.handle("system.update_config", params, &f.admin).await);
        assert_eq!(config(&f), before);
    }
    // Other generic config adapters must not bypass the raw-editor refusal.
    let mut table = before.parse::<toml::Table>().unwrap();
    table.remove("skill_trust");
    let path = f.home.path().join("config.toml");
    assert!(super::super::config_commit::commit_table_locked(&path,
        super::super::config_commit::content_hash(&before), &table).await.is_err());
    assert_eq!(config(&f), before);
}

#[tokio::test]
async fn unpin_disables_auto_approved_skills() {
    let f = fixture().await;
    payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let (kept, digest) = promoted(&f, "kept");
    let (changed, changed_digest) = promoted(&f, "changed");
    let (_, manual_digest) = promoted(&f, "manual");
    let (_, other_digest) = promoted(&f, "other");
    approve(&f, "pack", json!([
        {"agent":"alice", "name":"kept", "digest":digest},
        {"agent":"alice", "name":"changed", "digest":changed_digest}]), &policy()).await;
    // Duplicate historical approvals count each skill once.
    approve(&f, "pack-again", json!([{"agent":"alice", "name":"kept", "digest":digest}]), &policy()).await;
    approve(&f, "manual", json!([{"agent":"alice", "name":"manual", "digest":manual_digest}]), "dashboard:admin").await;
    approve(&f, "other", json!([{"agent":"alice", "name":"other", "digest":other_digest}]), &format!("{}0", policy())).await;
    std::fs::write(changed.join("SKILL.md"), "changed since approval").unwrap();
    // No session, password, code or risk acknowledgement for unpin.
    let got = payload(f.handler.handle("skill_trust.unpin", json!({"publisher":"in-house", "key_id":key_id()}), &f.admin).await);
    assert_eq!(got, json!({"removed":1,"disabled":1}));
    let state = duduclaw_agent::skill_state::read_disabled(f.home.path(), "alice");
    assert!(state.is_disabled("kept"));
    for name in ["changed", "manual", "other"] { assert!(!state.is_disabled(name)); }
    assert!(kept.join("SKILL.md").is_file());
    assert!(changed.join("SKILL.md").is_file());
    assert_eq!(TrustStore::load(f.home.path()).unwrap().keys("in-house"), vec![OFFICIAL_PUBLISHER_KEY]);
    let rows = audit(&f, "skill_trust_unpinned");
    assert_eq!(rows[0]["details"]["disabled"], 1);
    assert_eq!(rows[0]["agent_id"], f.admin.user_id);
}

#[tokio::test]
async fn unpin_without_disabling_removes_only_policy() {
    let f = fixture().await;
    payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let (path, digest) = promoted(&f, "kept");
    approve(&f, "pack", json!([{"agent":"alice", "name":"kept", "digest":digest}]), &policy()).await;
    let args = json!({"publisher":"in-house", "key_id":key_id(), "disable_skills":false});
    let got = payload(f.handler.handle("skill_trust.unpin", args.clone(), &f.admin).await);
    assert_eq!(got, json!({"removed":1,"disabled":0}));
    assert!(!duduclaw_agent::skill_state::read_disabled(f.home.path(), "alice").is_disabled("kept"));
    assert!(path.join("SKILL.md").is_file());
    assert_eq!(TrustStore::load(f.home.path()).unwrap().keys("in-house"), vec![OFFICIAL_PUBLISHER_KEY]);
    assert_eq!(payload(f.handler.handle("skill_trust.unpin", args, &f.admin).await), json!({"removed":0,"disabled":0}));
}

#[tokio::test]
async fn get_lists_trust_state_and_twenty_newest_auto_approvals() {
    let f = fixture().await;
    payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let digest = "0".repeat(64);
    let raw = rusqlite::Connection::open(f.home.path().join("approvals.db")).unwrap();
    let mut ids = Vec::new();
    for n in 0..23 {
        let id = approve(&f, &format!("pack-{n}"), json!([{"agent":"alice","name":"test","digest":digest}]), &policy()).await;
        // Explicit times make the ordering independent of scheduler resolution.
        let time = (Utc::now() - chrono::Duration::minutes(23 - n)).to_rfc3339();
        raw.execute("UPDATE approvals SET decided_at=?1 WHERE id=?2", rusqlite::params![time, id.as_str()]).unwrap();
        ids.push(id);
    }
    approve(&f, "manual", json!([]), "dashboard:admin").await;
    let got = payload(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    assert_eq!(got["auto_approve"][0]["in_trust_store"], true);
    assert_eq!(got["auto_approve"][0]["added_by"], f.admin.user_id);
    assert_eq!(got["skill_ttl_days"], 9);
    let publisher = got["publishers"].as_array().unwrap().iter().find(|p| p["publisher"] == "in-house").unwrap();
    assert_eq!(publisher["keys"][0]["key_id"], key_id());
    assert_eq!(publisher["keys"][0]["public_key"], OFFICIAL_PUBLISHER_KEY);
    let recent = got["recent_auto_approvals"].as_array().unwrap();
    assert_eq!(recent.len(), 20);
    for (value, id) in recent.iter().zip(ids.iter().rev()) {
        assert_eq!(value["id"], id.as_str());
        assert_eq!(value["publisher"], "in-house");
        assert_eq!(value["key_id"], key_id());
        assert_eq!(value["skills"][0]["name"], "test");
    }
    std::fs::write(f.home.path().join(TRUST_STORE_FILE), "").unwrap();
    let got = payload(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    assert_eq!(got["auto_approve"][0]["in_trust_store"], false);
    // A corrupt history row must fail the read, not disappear from the list.
    raw.execute("UPDATE approvals SET payload='invalid-json' WHERE id=?1", [ids[0].as_str()]).unwrap();
    refused(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
}

#[tokio::test]
async fn skill_trust_read_errors_never_become_empty_success() {
    let f = fixture().await;
    let original = config(&f);
    for broken in ["[broken", "[skill_trust]\nauto_approve='broken'", "[skill_trust]\nunknown=true"] {
        std::fs::write(f.home.path().join("config.toml"), broken).unwrap();
        refused(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    }
    std::fs::write(f.home.path().join("config.toml"), &original).unwrap();
    std::fs::write(f.home.path().join(TRUST_STORE_FILE), "[broken").unwrap();
    refused(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    refused(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    assert_eq!(config(&f), original);
    assert_eq!(std::fs::read_to_string(f.home.path().join(TRUST_STORE_FILE)).unwrap(), "[broken");
}

#[test]
fn skill_trust_methods_are_available_in_personal_edition() {
    for method in ["skill_trust.get", "skill_trust.pin", "skill_trust.unpin"] {
        assert!(!super::super::is_enterprise_only_method(method));
    }
}
