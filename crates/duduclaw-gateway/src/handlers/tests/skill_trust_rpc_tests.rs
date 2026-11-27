use super::*;
use duduclaw_core::publishers::{TrustStore, OFFICIAL_PUBLISHER_KEY, TRUST_STORE_FILE};
use crate::approval::{ApprovalBroker, ApprovalId};
use crate::skill_trust::minisign_key_id;
use super::super::skill_trust_rpc::{live_browser_admin, pin, trust_store_sync_failures};

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
    let text = match std::fs::read_to_string(f.home.path().join("security_audit.jsonl")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("cannot read audit: {error}"),
    };
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()).filter(|row| row["event_type"] == event).collect()
}
fn consume_pin_proof(f: &Fixture, db: &UserDb, session: &str) {
    let now = Utc::now().timestamp();
    let proof = db.skill_trust_pin_reauthenticate(&f.admin.user_id, session, f.admin.credential_epoch,
        "test-password", "", &crate::mfa::Cipher(f.home.path()), now).unwrap();
    db.skill_trust_pin_consume(&f.admin.user_id, session, f.admin.credential_epoch, &proof, now).unwrap();
}
fn colliding_key() -> String {
    use base64::Engine;
    let mut bytes = base64::engine::general_purpose::STANDARD.decode(OFFICIAL_PUBLISHER_KEY).unwrap();
    // Preserve the algorithm and eight-byte id; change the public key itself.
    bytes[10] ^= 1;
    let key = base64::engine::general_purpose::STANDARD.encode(bytes);
    duduclaw_core::publishers::validate_pubkey(&key).unwrap();
    assert_ne!(key, OFFICIAL_PUBLISHER_KEY);
    assert_eq!(minisign_key_id(&key).unwrap(), key_id());
    key
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
    assert_eq!(rows[0]["details"]["durability_complete"], true);
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
async fn pin_config_cas_failure_never_changes_trust_store() {
    for existing_store in [false, true] {
        let f = fixture().await;
        let trust_path = f.home.path().join(TRUST_STORE_FILE);
        if existing_store {
            std::fs::write(&trust_path, format!("[publishers.other]\nkeys=['{OFFICIAL_PUBLISHER_KEY}']\n")).unwrap();
        }
        let trust_before = std::fs::read(&trust_path).ok();
        let db = f.handler.user_db.read().await.clone().unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        consume_pin_proof(&f, &db, &session);
        let concurrent = format!("{}# concurrent change\n", config(&f));
        let calls = std::cell::Cell::new(0);
        let error = pin(f.home.path(), &args(), &f.admin.user_id, || {
            calls.set(calls.get() + 1);
            assert_eq!(calls.get(), 1, "CAS must fail before the commit guard or trust-store write");
            std::fs::write(f.home.path().join("config.toml"), &concurrent).unwrap();
            live_browser_admin(&db, &f.admin, &session).map(|_| ())
        }).unwrap_err();
        assert_eq!(error, "無法儲存自家技能來源，請重新整理後再試。");
        assert_eq!(calls.get(), 1);
        assert_eq!(config(&f), concurrent);
        assert_eq!(std::fs::read(&trust_path).ok(), trust_before);
        assert!(audit(&f, "skill_trust_pinned").is_empty());
        assert!(audit(&f, "skill_trust_pin_incomplete").is_empty());
    }
}

#[tokio::test]
async fn pin_post_persist_sync_failure_reports_active_success_and_durability_warning() {
    let f = fixture().await;
    trust_store_sync_failures().lock().unwrap().insert(f.home.path().to_path_buf());
    let entry = payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    assert_eq!(entry["key_id"], key_id());
    assert!(!trust_store_sync_failures().lock().unwrap().contains(f.home.path()), "post-persist hook must run");
    assert_eq!(TrustStore::load(f.home.path()).unwrap().keys("in-house"), vec![OFFICIAL_PUBLISHER_KEY]);
    let rows = audit(&f, "skill_trust_pinned");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["agent_id"], f.admin.user_id);
    assert_eq!(rows[0]["details"]["publisher"], "in-house");
    assert_eq!(rows[0]["details"]["key_id"], key_id());
    assert_eq!(rows[0]["details"]["trust_store_changed"], true);
    assert_eq!(rows[0]["details"]["durability_complete"], false);
    assert!(audit(&f, "skill_trust_pin_incomplete").is_empty());
    let got = payload(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    assert_eq!(got["auto_approve"].as_array().unwrap().len(), 1);
    assert_eq!(got["auto_approve"][0]["in_trust_store"], true);
    assert_eq!(got["auto_approve"][0]["conflicting"], false);
}

#[tokio::test]
async fn pin_rechecks_admin_and_session_under_trust_store_lock_after_config_commit() {
    for revoke_session in [false, true] {
        let f = fixture().await;
        let db = f.handler.user_db.read().await.clone().unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        db.remember_create(&f.admin.user_id, f.admin.credential_epoch, &session, "test", None, 1,
            Utc::now().timestamp(), false, |_, _, _| Ok(())).unwrap();
        consume_pin_proof(&f, &db, &session);
        let calls = std::cell::Cell::new(0);
        let error = pin(f.home.path(), &args(), &f.admin.user_id, || {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                let table: toml::Table = config(&f).parse().unwrap();
                assert_eq!(table["skill_trust"]["auto_approve"].as_array().unwrap().len(), 1);
                assert!(!f.home.path().join(TRUST_STORE_FILE).exists());
                let lock = std::fs::OpenOptions::new().read(true).write(true)
                    .open(f.home.path().join(format!("{TRUST_STORE_FILE}.lock"))).unwrap();
                assert_eq!(fs2::FileExt::try_lock_exclusive(&lock).unwrap_err().raw_os_error(),
                    fs2::lock_contended_error().raw_os_error());
                if revoke_session {
                    db.remember_revoke(&f.admin.user_id, Some(&session)).unwrap();
                } else {
                    db.update_user(&f.admin.user_id, None, Some(UserRole::Employee), None).unwrap();
                }
            }
            live_browser_admin(&db, &f.admin, &session).map(|_| ())
        }).unwrap_err();
        assert_eq!(calls.get(), 3);
        assert_eq!(error, "來源已儲存，但尚未啟用，請重新整理後再試。");
        assert!(!f.home.path().join(TRUST_STORE_FILE).exists());
        assert!(TrustStore::load(f.home.path()).unwrap().keys("in-house").is_empty());
        assert!(audit(&f, "skill_trust_pinned").is_empty());
        let rows = audit(&f, "skill_trust_pin_incomplete");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_id"], f.admin.user_id);
        assert_eq!(rows[0]["details"]["publisher"], "in-house");
        assert_eq!(rows[0]["details"]["key_id"], key_id());
        // Inspect through the real get RPC with a separate, still-live Admin.
        db.update_user(&f.member.user_id, None, Some(UserRole::Admin), None).unwrap();
        let observer = db.live_user_context(&f.member.user_id).unwrap().unwrap();
        let got = payload(f.handler.handle("skill_trust.get", json!({}), &observer).await);
        assert_eq!(got["auto_approve"].as_array().unwrap().len(), 1);
        assert_eq!(got["auto_approve"][0]["in_trust_store"], false);
        assert_eq!(got["auto_approve"][0]["conflicting"], false);
    }
}

#[tokio::test]
async fn conflicting_key_id_refuses_pin_and_unpin_and_is_visible_in_get() {
    let f = fixture().await;
    payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
    let before = config(&f);
    let trust_before = std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap();
    let mut collision = args();
    collision["public_key"] = json!(colliding_key());
    refused(browser(&f, &f.admin, "skill_trust.pin", collision.clone()).await);
    assert_eq!(config(&f), before);
    assert_eq!(audit(&f, "skill_trust_pinned").len(), 1);
    // A hand-edited legacy config may already contain both keys.
    let mut table: toml::Table = before.parse().unwrap();
    let entries = table.get_mut("skill_trust").unwrap().get_mut("auto_approve").unwrap().as_array_mut().unwrap();
    let mut second = entries[0].clone();
    second.as_table_mut().unwrap().insert("key".into(), toml::Value::String(colliding_key()));
    entries.push(second);
    let conflicted = toml::to_string(&table).unwrap();
    std::fs::write(f.home.path().join("config.toml"), &conflicted).unwrap();
    let got = payload(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
    let entries = got["auto_approve"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry["conflicting"] == true));
    assert_eq!(entries[0]["in_trust_store"], true);
    assert_eq!(entries[1]["in_trust_store"], false);
    // Even re-pinning either existing key must refuse the ambiguous identity.
    for params in [args(), collision] {
        refused(browser(&f, &f.admin, "skill_trust.pin", params).await);
    }
    for disable in [true, false] {
        refused(f.handler.handle("skill_trust.unpin",
            json!({"publisher":"in-house", "key_id":key_id(), "disable_skills":disable}), &f.admin).await);
    }
    assert_eq!(config(&f), conflicted);
    assert_eq!(std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap(), trust_before);
    assert_eq!(audit(&f, "skill_trust_pinned").len(), 1);
    assert!(audit(&f, "skill_trust_unpinned").is_empty());
    assert!(audit(&f, "skill_trust_pin_incomplete").is_empty());
}

#[tokio::test]
async fn forged_or_invalid_key_poisoning_is_visible_and_refuses_pin_and_unpin() {
    use base64::Engine;
    for invalid_key in [false, true] {
        let f = fixture().await;
        payload(browser(&f, &f.admin, "skill_trust.pin", args()).await);
        let trust_before = std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap();
        let mut table: toml::Table = config(&f).parse().unwrap();
        let entries = table.get_mut("skill_trust").unwrap().get_mut("auto_approve").unwrap().as_array_mut().unwrap();
        let mut second = entries[0].clone();
        let claimed_id = if invalid_key { key_id() } else { "0000000000000000".to_owned() };
        assert_ne!(key_id(), "0000000000000000");
        second.as_table_mut().unwrap().insert("key".into(),
            toml::Value::String(if invalid_key { "invalid".into() } else { colliding_key() }));
        second.as_table_mut().unwrap().insert("key_id".into(), toml::Value::String(claimed_id.clone()));
        entries.push(second);
        let mut pin_attempts = vec![args()];
        if !invalid_key {
            // Also pin a valid key under the forged claim: both identities
            // must be poisoned, not just the collision's canonical id.
            let mut bytes = base64::engine::general_purpose::STANDARD.decode(OFFICIAL_PUBLISHER_KEY).unwrap();
            bytes[2..10].fill(0);
            let claimed_key = base64::engine::general_purpose::STANDARD.encode(bytes);
            assert_eq!(minisign_key_id(&claimed_key).unwrap(), claimed_id);
            let mut third = entries[0].clone();
            third.as_table_mut().unwrap().insert("key".into(), toml::Value::String(claimed_key.clone()));
            third.as_table_mut().unwrap().insert("key_id".into(), toml::Value::String(claimed_id.clone()));
            entries.push(third);
            for key in [colliding_key(), claimed_key] {
                let mut params = args();
                params["public_key"] = json!(key);
                pin_attempts.push(params);
            }
        }
        let poisoned = toml::to_string(&table).unwrap();
        std::fs::write(f.home.path().join("config.toml"), &poisoned).unwrap();
        let got = payload(f.handler.handle("skill_trust.get", json!({}), &f.admin).await);
        let entries = got["auto_approve"].as_array().unwrap();
        assert_eq!(entries.len(), if invalid_key { 2 } else { 3 });
        assert!(entries.iter().all(|entry| entry["conflicting"] == true));
        assert_eq!(entries[0]["in_trust_store"], true);
        for params in pin_attempts {
            refused(browser(&f, &f.admin, "skill_trust.pin", params).await);
        }
        for id in [key_id(), claimed_id] {
            for disable in [true, false] {
                refused(f.handler.handle("skill_trust.unpin",
                    json!({"publisher":"in-house", "key_id":id, "disable_skills":disable}), &f.admin).await);
            }
        }
        assert_eq!(config(&f), poisoned);
        assert_eq!(std::fs::read(f.home.path().join(TRUST_STORE_FILE)).unwrap(), trust_before);
        assert_eq!(audit(&f, "skill_trust_pinned").len(), 1);
        assert!(audit(&f, "skill_trust_unpinned").is_empty());
        assert!(audit(&f, "skill_trust_pin_incomplete").is_empty());
    }
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
    assert_eq!(got["auto_approve"][0]["conflicting"], false);
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
    let table: toml::Table = config(&f).parse().unwrap();
    assert_eq!(table["skill_trust"]["auto_approve"].as_array().unwrap().len(), 1);
    assert_eq!(audit(&f, "skill_trust_pin_incomplete").len(), 1);
    assert!(audit(&f, "skill_trust_pinned").is_empty());
    assert_eq!(std::fs::read_to_string(f.home.path().join(TRUST_STORE_FILE)).unwrap(), "[broken");
}

#[test]
fn skill_trust_methods_are_available_in_personal_edition() {
    for method in ["skill_trust.get", "skill_trust.pin", "skill_trust.unpin"] {
        assert!(!super::super::is_enterprise_only_method(method));
    }
}
