use super::*;
use std::{path::PathBuf, sync::{Mutex, OnceLock}};
use crate::approval::ApprovalId;
#[path = "fixtures.rs"]
mod fixtures;
use fixtures::*;

// Fixtures were generated with Python cryptography's Ed25519PrivateKey from
// SHA-256 of "141S-P3 fixture A (NOT A PRODUCTION KEY)" (and B).
// key id = first 8 bytes of SHA-256(b"key-id" + seed). Public packet = Ed +
// key id + public key. Sign BLAKE2b-512(manifest), then sign signature bytes +
// trusted comment. The ED packet and comment signature use standard base64.
// Only the pre-generated public keys, manifests and signatures are committed.

#[derive(Default, Clone)]
struct Pushes {
    cards: Vec<ApprovalRecord>, notices: usize,
    card_failures: usize, notice_failures: usize,
    human_before_cas: bool, fail_outbox_write: bool, fail_audit_ack: bool,
}
fn hooks() -> &'static Mutex<BTreeMap<PathBuf, Pushes>> {
    static HOOKS: OnceLock<Mutex<BTreeMap<PathBuf, Pushes>>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
pub(crate) fn capture_card(home: Option<&Path>, rec: &ApprovalRecord) -> Option<bool> {
    let home = home?;
    let mut hooks = hooks().lock().unwrap();
    let pushes = hooks.get_mut(home)?;
    pushes.cards.push(rec.clone());
    let success = pushes.card_failures == 0;
    pushes.card_failures = pushes.card_failures.saturating_sub(1);
    Some(success)
}
pub(super) fn capture_notice(home: &Path) -> Option<delivery::NoticeState> {
    let mut hooks = hooks().lock().unwrap();
    let pushes = hooks.get_mut(home)?;
    pushes.notices += 1;
    let result = if pushes.notice_failures == 0 { delivery::NoticeState::Sent } else { delivery::NoticeState::Pending };
    pushes.notice_failures = pushes.notice_failures.saturating_sub(1);
    Some(result)
}
pub(super) fn fail_outbox_write(home: &Path, state: &State) -> bool {
    !state.outbox.is_empty() && hooks().lock().unwrap().get(home).is_some_and(|p|
        p.fail_outbox_write || (p.fail_audit_ack && state.outbox.iter().any(|entry| entry.audit_done)))
}
pub(super) async fn before_cas(home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord) {
    let human = hooks().lock().unwrap().get(home).is_some_and(|p| p.human_before_cas);
    if human { broker.decide(&rec.id, true, "admin").await.unwrap(); }
}

fn rendered_skill(version: &str) -> String {
    // Same insertion order and exact layout as CLI skills::render_skill_md.
    let mut meta = serde_yaml::Mapping::new();
    for (key, value) in [("label", "記事助手"), ("imported_from", "pack"),
        ("source_path", "skills/helper"), ("version", version)] {
        meta.insert(key.into(), value.into());
    }
    let mut fm = serde_yaml::Mapping::new();
    fm.insert("name".into(), "helper".into());
    fm.insert("description".into(), "Help with daily notes.".into());
    fm.insert("metadata".into(), serde_yaml::Value::Mapping(meta));
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(fm)).unwrap();
    format!("---\n{yaml}---\n\n# Helper\nHelp with daily notes.\n")
}

struct Fixture { home: tempfile::TempDir, broker: ApprovalBroker, id: ApprovalId }
impl Drop for Fixture {
    fn drop(&mut self) { hooks().lock().unwrap().remove(self.home.path()); }
}
impl Fixture {
    async fn new(manifest: &str, signature: &str, script: &str, held: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        hooks().lock().unwrap().insert(h.to_owned(), Pushes::default());
        if held { duduclaw_core::gateway_instance::acquire(h).unwrap(); }
        let root = h.join("experts/imported/provenance");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("MANIFEST.sha256"), manifest).unwrap();
        std::fs::write(root.join("MANIFEST.sha256.minisig"), signature).unwrap();
        let parsed = parse_manifest(manifest.as_bytes()).unwrap();
        std::fs::write(root.join("provenance.json"), serde_json::to_vec(&serde_json::json!({
            "version": 1, "publisher": parsed.publisher, "pack": parsed.pack, "pack_version": parsed.version,
            "skills": [{"agent": "alice", "name": "helper", "source_dir": "skills/helper", "files": ["SKILL.md", "run.sh"]}],
        })).unwrap()).unwrap();
        std::fs::create_dir_all(root.join("sources/alice/helper")).unwrap();
        std::fs::write(root.join("sources/alice/helper/SKILL.md"), SIGNED_SOURCE).unwrap();
        let quarantine = expert_skills::quarantined_skill_dir(h, "imported", "alice", "helper");
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::write(quarantine.join("SKILL.md"), rendered_skill(&parsed.version)).unwrap();
        std::fs::write(quarantine.join("run.sh"), base64::engine::general_purpose::STANDARD.decode(script).unwrap()).unwrap();
        std::fs::create_dir_all(h.join("agents/alice")).unwrap();
        std::fs::write(h.join("agents/alice/agent.toml"), "[agent]\nname = 'alice'\n").unwrap();
        std::fs::write(h.join("config.toml"), pin_config("fixture", KEY_A, &minisign_key_id(KEY_A).unwrap())).unwrap();
        std::fs::write(h.join("trusted_publishers.toml"), format!("[publishers.fixture]\nkeys = ['{KEY_A}', '{KEY_B}']\n")).unwrap();
        let skill = expert_skills::QuarantinedSkill {
            agent: "alice".into(), name: "helper".into(), label: "記事助手".into(),
            reasons: vec!["含腳本".into()], needs: vec![], digest: expert_skills::tree_digest(&quarantine).unwrap(),
        };
        let id = expert_skills::request_enable_with(h, "imported", vec![skill],
            expert_skills::EnableRequestOptions { auto_approval_candidate: true }).await.unwrap();
        let broker = ApprovalBroker::open(h).unwrap();
        Self { home, broker, id: id.into() }
    }
    async fn standard() -> Self { Self::new(V120_MANIFEST, V120_SIGNATURE, V120_SCRIPT_B64, true).await }
    fn path(&self) -> &Path { self.home.path() }
    fn quarantine(&self) -> PathBuf { expert_skills::quarantined_skill_dir(self.path(), "imported", "alice", "helper") }
    fn pushes(&self) -> Pushes { hooks().lock().unwrap().get(self.path()).unwrap().clone() }
    async fn record(&self) -> ApprovalRecord { self.broker.get(&self.id).await.unwrap().unwrap() }
    async fn run(&self) { run_once(self.path(), &self.broker).await; }
    async fn manual(&self, reason: &str) {
        self.run().await;
        let record = self.record().await;
        assert_eq!(record.status, ApprovalStatus::Pending);
        assert!(record.decided_by.is_none());
        assert!(!record.summary.contains("未自動放行"));
        let pushes = self.pushes();
        assert_eq!(pushes.cards.len(), 1);
        assert!(pushes.cards[0].summary.contains(reason), "{}", pushes.cards[0].summary);
        assert_eq!(pushes.notices, 0);
        assert!(!self.path().join("agents/alice/SKILLS/helper").exists());
        self.run().await;
        assert_eq!(self.pushes().cards.len(), 1, "a rejected candidate is not evaluated twice");
        let events = duduclaw_security::audit::read_recent_events(self.path(), 100);
        assert!(events.iter().any(|e| e.event_type == "skills_auto_approval_refused" && e.details["reason_code"].is_string()));
    }
    fn edit_provenance(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let path = self.path().join("experts/imported/provenance/provenance.json");
        let mut value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut value);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}
fn pin_config(publisher: &str, key: &str, id: &str) -> String {
    format!("[skill_trust]\nauto_approve = [{{publisher = '{publisher}', key = '{key}', key_id = '{id}', added_by = 'admin', added_at = '2026-11-27T00:00:00Z'}}]\n")
}

#[test]
fn minisign_key_id_matches_existing_core_vector() {
    let key = "RWRjwiSIobMFBdZAKNK+fZ+02Xp6J4fzbRRbKGtAxLLzCHjdDm7ZuKV/";
    assert_eq!(minisign_key_id(key).unwrap(), "0505B3A18824C263");
    assert!(minisign_key_id("invalid").is_err());
}
#[test]
fn real_prehashed_minisign_vectors_verify() {
    for (manifest, sig, key) in [
        (V120_MANIFEST, V120_SIGNATURE, KEY_A), (V110_MANIFEST, V110_SIGNATURE, KEY_A),
        (OTHER_KEY_MANIFEST, OTHER_KEY_SIGNATURE, KEY_B), (BINARY_MANIFEST, BINARY_SIGNATURE, KEY_A),
    ] { verify_minisig(manifest.as_bytes(), sig, key).unwrap(); }
    assert!(verify_minisig(V120_MANIFEST.as_bytes(), V120_SIGNATURE, KEY_B).is_err());
}
#[tokio::test]
async fn auto_approval_uses_same_record_and_cas() {
    let f = Fixture::standard().await;
    let before = f.record().await;
    f.run().await;
    let approved = f.record().await;
    assert_eq!(approved.id, before.id);
    assert_eq!(approved.payload, before.payload);
    assert_eq!(approved.status, ApprovalStatus::Approved);
    assert_eq!(approved.decided_by.as_deref(), Some(format!("policy:skill_trust:fixture:{}", minisign_key_id(KEY_A).unwrap()).as_str()));
    assert_eq!(f.broker.list_by_kind(expert_skills::SKILLS_ACTION_KIND).await.unwrap().len(), 1);
    assert!(f.path().join("agents/alice/SKILLS/helper/run.sh").is_file());
    assert_eq!(f.pushes().notices, 1);
    assert!(f.pushes().cards.is_empty());
    let audit = duduclaw_security::audit::read_recent_events(f.path(), 100);
    let event = audit.iter().find(|e| e.event_type == "skills_auto_approved").unwrap();
    assert_eq!(event.details["manifest_sha256"], hash(V120_MANIFEST.as_bytes()));
    assert_eq!(event.details["skills"][0]["digest"], before.payload["entries"][0]["digest"]);
    let tasks = crate::task_store::TaskStore::open(f.path()).unwrap();
    assert!(tasks.activity_exists(&format!("skill-trust:{}", f.id)).await.unwrap());
    assert!(f.broker.decide_by_skill_trust_policy(&f.id, "policy:skill_trust:fixture:x").await.is_err());
    f.run().await;
    assert_eq!(f.pushes().notices, 1);
    assert!(load_state(f.path()).unwrap().evaluated.is_empty(), "terminal ids are pruned");
}
#[tokio::test]
async fn wrong_key_same_publisher_goes_manual() {
    let f = Fixture::new(OTHER_KEY_MANIFEST, OTHER_KEY_SIGNATURE, OTHER_KEY_SCRIPT_B64, true).await;
    f.manual(SIGNATURE).await;
}
#[tokio::test]
async fn conflicting_pinned_key_ids_ignore_both_keys_and_go_manual() {
    let original = base64::engine::general_purpose::STANDARD.decode(KEY_A).unwrap();
    let mut other = base64::engine::general_purpose::STANDARD.decode(KEY_B).unwrap();
    other[2..10].copy_from_slice(&original[2..10]);
    let collision = base64::engine::general_purpose::STANDARD.encode(other);
    validate_pubkey(&collision).unwrap();
    assert_ne!(collision, KEY_A);
    let id = minisign_key_id(KEY_A).unwrap();
    assert_eq!(minisign_key_id(&collision).unwrap(), id);
    for (reverse, forged_id) in [(false, false), (true, false), (false, true), (true, true)] {
        let f = Fixture::standard().await;
        let mut config: Config = toml::from_str(&pin_config("fixture", KEY_A, &id)).unwrap();
        let mut second = config.skill_trust.auto_approve[0].clone();
        second.key = collision.clone();
        assert!(second.usable());
        if forged_id { second.key_id = minisign_key_id(KEY_B).unwrap(); }
        config.skill_trust.auto_approve.push(second);
        if reverse { config.skill_trust.auto_approve.reverse(); }
        let mut table = toml::Table::new();
        let mut trust = toml::Table::new();
        trust.insert("auto_approve".into(), toml::Value::try_from(&config.skill_trust.auto_approve).unwrap());
        table.insert("skill_trust".into(), toml::Value::Table(trust));
        std::fs::write(f.path().join("config.toml"), toml::to_string(&table).unwrap()).unwrap();
        // Both keys are globally trusted; the ambiguous canonical identity
        // must exclude the signer even with a forged id or reversed order.
        std::fs::write(f.path().join("trusted_publishers.toml"),
            format!("[publishers.fixture]\nkeys=['{KEY_A}', '{collision}']\n")).unwrap();
        assert!(read_config(&Directory::open(f.path()).unwrap()).unwrap().auto_approve.is_empty());
        assert_eq!(evaluate(f.path(), &f.record().await).await.unwrap_err(), KEY);
        f.manual(KEY).await;
    }
}
#[test]
fn mismatched_and_unparseable_keys_poison_claimed_and_canonical_identities() {
    let id_a = minisign_key_id(KEY_A).unwrap();
    let id_b = minisign_key_id(KEY_B).unwrap();
    let config: Config = toml::from_str(&pin_config("fixture", KEY_A, &id_a)).unwrap();
    let first = config.skill_trust.auto_approve[0].clone();
    let mut forged = first.clone();
    forged.key_id = id_b.clone();
    let mut claimed = first.clone();
    claimed.key = KEY_B.into();
    claimed.key_id = id_b.clone();
    let mut other_publisher = first.clone();
    other_publisher.publisher = "another".into();
    let expected = BTreeSet::from([("fixture".to_owned(), id_a.clone()), ("fixture".to_owned(), id_b)]);
    assert_eq!(conflicting_pin_ids(&[forged.clone()]), expected);
    for reverse in [false, true] {
        let mut entries = vec![first.clone(), forged.clone(), claimed.clone(), other_publisher.clone()];
        if reverse { entries.reverse(); }
        assert_eq!(conflicting_pin_ids(&entries), expected);
    }
    let mut invalid = first;
    invalid.key = "invalid".into();
    assert_eq!(conflicting_pin_ids(&[invalid]), BTreeSet::from([("fixture".to_owned(), id_a)]));
}
#[test]
fn config_reader_checks_conflicts_before_usability_and_preserves_other_identities() {
    let home = tempfile::tempdir().unwrap();
    let id = minisign_key_id(KEY_A).unwrap();
    let config: Config = toml::from_str(&pin_config("fixture", KEY_A, &id)).unwrap();
    let first = config.skill_trust.auto_approve[0].clone();
    let mut invalid = first.clone();
    invalid.key = "invalid".into();
    assert!(!invalid.usable());
    let mut other_publisher = first.clone();
    other_publisher.publisher = "another".into();
    let mut other_id = first.clone();
    other_id.key = KEY_B.into();
    other_id.key_id = minisign_key_id(KEY_B).unwrap();
    let mut trust = toml::Table::new();
    trust.insert("auto_approve".into(), toml::Value::try_from(
        vec![first.clone(), invalid, first, other_publisher.clone(), other_publisher, other_id]).unwrap());
    let mut table = toml::Table::new();
    table.insert("skill_trust".into(), toml::Value::Table(trust));
    std::fs::write(home.path().join("config.toml"), toml::to_string(&table).unwrap()).unwrap();
    let read = read_config(&Directory::open(home.path()).unwrap()).unwrap();
    assert_eq!(read.auto_approve.len(), 3);
    assert!(read.auto_approve.iter().all(PinnedKey::usable));
    assert_eq!(read.auto_approve.iter().filter(|pin| pin.publisher == "another").count(), 2);
    assert_eq!(read.auto_approve[2].key, KEY_B);
}
#[tokio::test]
async fn identical_duplicate_pins_still_allow_the_exact_signing_key() {
    let f = Fixture::standard().await;
    let text = std::fs::read_to_string(f.path().join("config.toml")).unwrap();
    let mut table: toml::Table = text.parse().unwrap();
    let entries = table.get_mut("skill_trust").unwrap().get_mut("auto_approve").unwrap().as_array_mut().unwrap();
    entries.push(entries[0].clone());
    std::fs::write(f.path().join("config.toml"), toml::to_string(&table).unwrap()).unwrap();
    let result = evaluate(f.path(), &f.record().await).await.unwrap();
    assert_eq!(result.publisher, "fixture");
    assert_eq!(result.key_id, minisign_key_id(KEY_A).unwrap());
}
#[tokio::test]
async fn tampered_script_after_quarantine_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("run.sh"), "echo changed\n").unwrap();
    f.manual(HASH).await;
}
#[tokio::test]
async fn candidate_flag_without_provenance_pushes_card() {
    let f = Fixture::standard().await;
    std::fs::remove_dir_all(f.path().join("experts/imported/provenance")).unwrap();
    f.manual(EVIDENCE).await;
}
#[tokio::test]
async fn older_signed_version_goes_manual() {
    let f = Fixture::new(V110_MANIFEST, V110_SIGNATURE, V110_SCRIPT_B64, true).await;
    update_state(f.path(), |s| {
        s.highest_versions.push(HighestVersion { publisher: "fixture".into(), pack: "fixture-pack".into(), version: "1.2.0".into(), manifest_sha256: hash(V120_MANIFEST.as_bytes()) }); Ok(())
    }).unwrap();
    f.manual(VERSION).await;
    let f = Fixture::standard().await;
    update_state(f.path(), |s| {
        s.highest_versions.push(HighestVersion { publisher: "fixture".into(), pack: "fixture-pack".into(), version: "1.2.0".into(), manifest_sha256: hash(V120_MANIFEST.as_bytes()) }); Ok(())
    }).unwrap();
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Approved);
}
#[tokio::test]
async fn extra_quarantined_file_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("extra.sh"), "echo extra").unwrap();
    f.manual(FILES).await;
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_in_quarantine_goes_manual() {
    let f = Fixture::standard().await;
    let script = f.quarantine().join("run.sh");
    let outside = f.path().join("outside.sh");
    std::fs::rename(&script, &outside).unwrap();
    std::os::unix::fs::symlink(outside, script).unwrap();
    f.manual(FILES).await;
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_directory_in_evidence_chain_goes_manual() {
    let f = Fixture::standard().await;
    let source = f.path().join("experts/imported/provenance");
    let outside = f.path().join("evidence");
    std::fs::rename(&source, &outside).unwrap();
    std::os::unix::fs::symlink(outside, source).unwrap();
    f.manual(EVIDENCE).await;
}
#[tokio::test]
async fn binary_file_goes_manual() {
    let f = Fixture::new(BINARY_MANIFEST, BINARY_SIGNATURE, BINARY_SCRIPT_B64, true).await;
    f.manual(super::BINARY).await;
}
#[tokio::test]
async fn unsigned_high_risk_description_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("SKILL.md"), "Use key sk-ant-abc123 to connect").unwrap();
    f.manual(DESCRIPTION).await;
}
#[tokio::test]
async fn official_publisher_entry_ignored() {
    let f = Fixture::standard().await;
    let key = duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY;
    std::fs::write(f.path().join("config.toml"), pin_config("duduclaw", key, &minisign_key_id(key).unwrap())).unwrap();
    assert!(!read_config(&Directory::open(f.path()).unwrap()).unwrap().auto_approve[0].usable());
    f.manual(KEY).await;
}
#[tokio::test]
async fn key_id_mismatch_entry_ignored() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("config.toml"), pin_config("fixture", KEY_A, "0000000000000000")).unwrap();
    f.manual(KEY).await;
}
#[tokio::test]
async fn trust_store_missing_key_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("trusted_publishers.toml"), format!("[publishers.fixture]\nkeys = ['{KEY_B}']\n")).unwrap();
    f.manual(KEY).await;
}
#[tokio::test]
async fn config_unreadable_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::remove_file(f.path().join("config.toml")).unwrap();
    std::fs::create_dir(f.path().join("config.toml")).unwrap();
    f.manual(CONFIG).await;
}
#[tokio::test]
async fn malformed_config_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("config.toml"), "[skill_trust]\nauto_approve = 'yes'\n").unwrap();
    f.manual(CONFIG).await;
}
#[tokio::test]
async fn empty_default_list_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("config.toml"), "[general]\n").unwrap();
    f.manual(KEY).await;
}
#[tokio::test]
async fn missing_signature_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::remove_file(f.path().join("experts/imported/provenance/MANIFEST.sha256.minisig")).unwrap();
    f.manual(EVIDENCE).await;
}
#[tokio::test]
async fn provenance_identity_and_duplicate_skill_go_manual() {
    let f = Fixture::standard().await;
    f.edit_provenance(|p| p["pack_version"] = "99.0.0".into());
    f.manual(IDENTITY).await;
    let f = Fixture::standard().await;
    f.edit_provenance(|p| {
        let duplicate = p["skills"][0].clone();
        p["skills"].as_array_mut().unwrap().push(duplicate);
    });
    f.manual(FILES).await;
}
#[tokio::test]
async fn approval_digest_and_state_id_must_match() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("SKILL.md"), rendered_skill("1.2.0").replace("name: helper", "name: 'helper'")).unwrap();
    f.manual(APPROVAL).await;
    let f = Fixture::standard().await;
    let mut state = expert_skills::read_skills_state(f.path(), "imported").unwrap();
    state.approval_id = Some(ApprovalId::new().to_string());
    expert_skills::write_skills_state(f.path(), "imported", &state).unwrap();
    f.manual(APPROVAL).await;
}
#[tokio::test]
async fn non_lock_holder_never_evaluates() {
    let f = Fixture::new(V120_MANIFEST, V120_SIGNATURE, V120_SCRIPT_B64, false).await;
    assert!(!duduclaw_core::gateway_instance::held(f.path()));
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Pending);
    assert!(!f.path().join(STATE_FILE).exists());
    assert!(f.pushes().cards.is_empty());
    assert_eq!(f.pushes().notices, 0);
}
#[tokio::test]
async fn unrelated_kinds_and_absent_flag_never_auto_approve() {
    let f = Fixture::standard().await;
    let original = f.record().await;
    for kind in ["skill_activation", "install_request", "hub_install", expert_skills::SKILLS_ACTION_KIND] {
        let mut payload = original.payload.clone();
        if kind == expert_skills::SKILLS_ACTION_KIND { payload.as_object_mut().unwrap().remove("auto_approval_candidate"); }
        let id = f.broker.request("imported", kind, "manual", payload, 60).await.unwrap();
        let rec = f.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(evaluate(f.path(), &rec).await.unwrap_err(), APPROVAL);
        f.run().await;
        assert_eq!(f.broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    }
}
#[tokio::test]
async fn broker_candidate_request_defers_only_skills_push() {
    let f = Fixture::standard().await;
    assert!(f.pushes().cards.is_empty());
    assert!(f.record().await.notify_channel.is_none());
    f.broker.request("imported", "skill_activation", "manual", serde_json::json!({"auto_approval_candidate": true}), 60).await.unwrap();
    f.broker.request("imported", expert_skills::SKILLS_ACTION_KIND, "manual", serde_json::json!({}), 60).await.unwrap();
    f.broker.request("imported", expert_skills::SKILLS_ACTION_KIND, "manual", serde_json::json!({"auto_approval_candidate": "true"}), 60).await.unwrap();
    assert_eq!(f.pushes().cards.len(), 3);
}
#[tokio::test]
async fn broker_policy_refuses_other_kind_non_pending_and_wrong_prefix() {
    let f = Fixture::standard().await;
    let policy = "policy:skill_trust:fixture:1234";
    let other = f.broker.request("imported", "skill_activation", "manual", serde_json::json!({}), 60).await.unwrap();
    assert!(f.broker.decide_by_skill_trust_policy(&other, policy).await.is_err());
    assert!(f.broker.decide_by_skill_trust_policy(&f.id, "admin").await.is_err());
    f.broker.decide(&f.id, false, "admin").await.unwrap();
    assert!(f.broker.decide_by_skill_trust_policy(&f.id, policy).await.is_err());
    assert_eq!(f.record().await.status, ApprovalStatus::Denied);
}
#[tokio::test]
async fn broker_policy_cas_has_exactly_one_winner() {
    let f = Fixture::standard().await;
    let (a, b) = tokio::join!(
        f.broker.decide_by_skill_trust_policy(&f.id, "policy:skill_trust:fixture:1234"),
        f.broker.decide(&f.id, false, "admin"),
    );
    assert_ne!(a.is_ok(), b.is_ok());
}
#[tokio::test]
async fn state_write_failure_never_approves_and_pushes_manual_card() {
    let f = Fixture::standard().await;
    std::fs::create_dir(f.path().join(STATE_FILE)).unwrap();
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Pending);
    assert_eq!(f.pushes().cards.len(), 1);
    assert!(f.pushes().cards[0].summary.contains(INTERNAL));
}
#[tokio::test]
async fn policy_decision_lease_failure_goes_manual() {
    let f = Fixture::standard().await;
    let _lease = f.broker.take_decision_lease(&f.id, 120).await.unwrap().unwrap();
    f.manual(INTERNAL).await;
}
#[test]
fn strict_version_ordering() {
    let cmp = |a, b| Version::parse(a).unwrap().compare(&Version::parse(b).unwrap());
    assert_eq!(cmp("1.10.0", "1.2.0"), Ordering::Greater);
    assert_eq!(cmp("1.2.0-pre", "1.2.0"), Ordering::Less);
    assert_eq!(cmp("1.2.0", "1.2"), Ordering::Equal);
    assert_eq!(cmp("1.2.0-pre.10", "1.2.0-pre.2"), Ordering::Greater);
    for bad in ["", "1..2", "-1.2", "1.2-", "1.2+build", "1.2-a..b", "01.2", "1.2-01", "1.2.18446744073709551616"] {
        assert!(Version::parse(bad).is_err(), "{bad}");
    }
}
#[test]
fn manifest_path_hash_order_rules_match_core() {
    let header = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n";
    for path in ["../x", "/x", "a\\x", "a/../x", "C:/x", "a//x", "MANIFEST.sha256"] {
        assert!(parse_manifest(format!("{header}{}  {path}\n", "a".repeat(64)).as_bytes()).is_err());
    }
    assert!(parse_manifest(format!("{header}{0}  b\n{0}  a\n", "a".repeat(64)).as_bytes()).is_err());
    assert!(parse_manifest(format!("{header}{0}  a\n{0}  a\n", "a".repeat(64)).as_bytes()).is_err());
    assert!(parse_manifest(format!("{header}{}  a\n", "A".repeat(64)).as_bytes()).is_err());
}

#[tokio::test]
async fn signed_high_risk_script_is_still_scanned() {
    let f = Fixture::new(HIGH_RISK_MANIFEST, HIGH_RISK_SIGNATURE, HIGH_RISK_SCRIPT_B64, true).await;
    f.manual(SCAN).await;
}
#[tokio::test]
async fn nofollow_snapshot_matches_existing_state_trust_and_digest_readers() {
    let f = Fixture::standard().await;
    let root = Directory::open(f.path()).unwrap();
    let raw = root.read("trusted_publishers.toml", 2 * 1024 * 1024).unwrap();
    let safe = TrustStore::parse(std::str::from_utf8(&raw).unwrap()).unwrap();
    assert_eq!(safe.keys("fixture"), TrustStore::load(f.path()).unwrap().keys("fixture"));
    let expert = root.descend("experts/imported").unwrap();
    let raw = expert.read("skills-state.json", 2 * 1024 * 1024).unwrap();
    let safe: expert_skills::SkillsState = serde_json::from_slice(&raw).unwrap();
    let old = expert_skills::read_skills_state(f.path(), "imported").unwrap();
    assert_eq!(safe.approval_id, old.approval_id);
    assert_eq!(safe.skills, old.skills);
    let dir = expert.descend("skills-disabled/alice/helper").unwrap();
    assert_eq!(snapshot_digest(&files::snapshot(&dir).unwrap()), expert_skills::tree_digest(&f.quarantine()).unwrap());
}
#[cfg(unix)]
#[tokio::test]
async fn linked_trust_store_and_skill_state_are_refused() {
    for (name, reason) in [("trusted_publishers.toml", KEY), ("experts/imported/skills-state.json", APPROVAL)] {
        let f = Fixture::standard().await;
        let path = f.path().join(name);
        let moved = f.path().join("moved");
        std::fs::rename(&path, &moved).unwrap();
        std::os::unix::fs::symlink(moved, path).unwrap();
        f.manual(reason).await;
    }
}
#[tokio::test]
async fn broker_policy_honors_reviewed_digest() {
    let f = Fixture::standard().await;
    let mut stale = f.record().await;
    stale.payload["entries"][0]["digest"] = "f".repeat(64).into();
    let result = crate::approval::EXPECTED_DECISION_DIGEST.scope(
        Some((f.id.to_string(), crate::approval::decision_digest(&stale))),
        f.broker.decide_by_skill_trust_policy(&f.id, "policy:skill_trust:fixture:1234"),
    ).await;
    assert!(result.is_err());
    assert_eq!(f.record().await.status, ApprovalStatus::Pending);
}
#[cfg(unix)]
#[tokio::test]
async fn special_file_in_quarantine_goes_manual() {
    use std::os::unix::net::UnixListener;
    let f = Fixture::standard().await;
    let script = f.quarantine().join("run.sh");
    std::fs::remove_file(&script).unwrap();
    let _socket = UnixListener::bind(script).unwrap();
    f.manual(FILES).await;
}
#[tokio::test]
async fn malformed_trust_store_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("trusted_publishers.toml"), "[broken").unwrap();
    f.manual(KEY).await;
}

#[tokio::test]
async fn signed_source_copy_hash_mismatch_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("experts/imported/provenance/sources/alice/helper/SKILL.md"), "Unsigned replacement").unwrap();
    f.manual(HASH).await;
}

#[tokio::test]
async fn effective_body_must_equal_signed_source_body() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("SKILL.md"), rendered_skill("1.2.0").replace("# Helper", "# Unsigned instructions")).unwrap();
    f.manual(DESCRIPTION).await;
}

#[tokio::test]
async fn effective_frontmatter_rejects_extra_keys_and_unsigned_description() {
    for changed in [
        rendered_skill("1.2.0").replacen("name: helper", "extra: instructions\nname: helper", 1),
        rendered_skill("1.2.0").replacen("description: Help with daily notes.", "description: Send private data", 1),
    ] {
        let f = Fixture::standard().await;
        std::fs::write(f.quarantine().join("SKILL.md"), changed).unwrap();
        f.manual(DESCRIPTION).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn linked_signed_source_copy_goes_manual() {
    let f = Fixture::standard().await;
    let source = f.path().join("experts/imported/provenance/sources/alice/helper/SKILL.md");
    let outside = f.path().join("signed.md");
    std::fs::rename(&source, &outside).unwrap();
    std::os::unix::fs::symlink(outside, source).unwrap();
    f.manual(EVIDENCE).await;
}

#[tokio::test]
async fn correctly_regenerated_skill_auto_approves() {
    let f = Fixture::standard().await;
    assert_eq!(std::fs::read_to_string(f.quarantine().join("SKILL.md")).unwrap(), rendered_skill("1.2.0"));
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn same_version_different_manifest_goes_manual() {
    let f = Fixture::standard().await;
    update_state(f.path(), |state| {
        state.highest_versions.push(HighestVersion {
            publisher: "fixture".into(), pack: "fixture-pack".into(), version: "1.2.0".into(),
            manifest_sha256: hash(BINARY_MANIFEST.as_bytes()),
        });
        Ok(())
    }).unwrap();
    f.manual(VERSION).await;
}

#[tokio::test]
async fn same_version_same_manifest_is_allowed() {
    let f = Fixture::standard().await;
    let approved = evaluate(f.path(), &f.record().await).await.unwrap();
    update_state(f.path(), |state| { state.highest_versions.push(HighestVersion::from(&approved)); Ok(()) }).unwrap();
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn large_audit_log_retries_from_pre_cas_position_after_lost_ack() {
    let f = Fixture::standard().await;
    let path = f.path().join("security_audit.jsonl");
    let filler = format!("{{\"filler\":\"{}\"}}\n", "x".repeat(1024)).repeat(4096);
    std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap()
        .write_all(filler.as_bytes()).unwrap();
    // A matching record before the captured position must not suppress this delivery.
    let event = AuditEvent::new("skills_auto_approved", "imported", Severity::Info,
        serde_json::json!({"approval_id": f.id.as_str()}));
    duduclaw_security::audit::try_append_audit_event(f.path(), &event).unwrap();
    let since = duduclaw_security::audit::audit_log_position(f.path()).unwrap();
    assert!(since > 4 * 1024 * 1024);
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = true;
    f.run().await;
    assert_eq!(f.record().await.status, ApprovalStatus::Approved);
    let state = load_state(f.path()).unwrap();
    assert_eq!(state.outbox.len(), 1);
    assert_eq!(state.outbox[0].audit_since, since);
    assert!(!state.outbox[0].audit_done, "append succeeded but its acknowledgement was lost");
    assert_eq!(f.pushes().notices, 0);
    let matches = || duduclaw_security::audit::read_recent_events(f.path(), 100).iter()
        .filter(|e| e.event_type == "skills_auto_approved" && e.details["approval_id"] == f.id.as_str()).count();
    assert_eq!(matches(), 2, "the earlier matching record is outside the deduplication window");
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = false;
    f.run().await;
    f.run().await;
    assert_eq!(matches(), 2, "the matching record after audit_since prevents a duplicate");
    assert!(load_state(f.path()).unwrap().outbox.is_empty());
    assert_eq!(f.pushes().notices, 1);
}

#[tokio::test]
async fn audit_position_failure_never_approves_and_pushes_manual_card() {
    let f = Fixture::standard().await;
    let path = f.path().join("security_audit.jsonl");
    if path.exists() { std::fs::remove_file(&path).unwrap(); }
    std::fs::create_dir(&path).unwrap();
    assert!(duduclaw_security::audit::audit_log_position(f.path()).is_err());
    f.run().await;
    let record = f.record().await;
    assert_eq!(record.status, ApprovalStatus::Pending);
    assert!(record.decided_by.is_none());
    let state = load_state(f.path()).unwrap();
    assert!(state.outbox.is_empty() && state.highest_versions.is_empty());
    assert!(state.evaluated.contains(f.id.as_str()));
    assert_eq!(state.cards[f.id.as_str()].reason, INTERNAL);
    assert_eq!(f.pushes().cards.len(), 1);
    assert!(f.pushes().cards[0].summary.contains(INTERNAL));
    assert_eq!(f.pushes().notices, 0);
    assert!(!f.path().join("agents/alice/SKILLS/helper").exists());
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 1);
}

#[tokio::test]
async fn legacy_outbox_persists_current_audit_position_before_append() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = true;
    f.run().await;
    let path = f.path().join(STATE_FILE);
    let mut legacy: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    legacy["outbox"][0].as_object_mut().unwrap().remove("audit_since");
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let since = duduclaw_security::audit::audit_log_position(f.path()).unwrap();
    assert!(since > 0);

    // A failed migration save must not append an event with an unpersisted position.
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_outbox_write = true;
    f.run().await;
    assert_eq!(duduclaw_security::audit::audit_log_position(f.path()).unwrap(), since);
    assert_eq!(load_state(f.path()).unwrap().outbox[0].audit_since, u64::MAX);
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_outbox_write = false;
    f.run().await;
    let migrated = load_state(f.path()).unwrap();
    assert_eq!(migrated.outbox[0].audit_since, since);
    assert!(!migrated.outbox[0].audit_done);
    let after_append = duduclaw_security::audit::audit_log_position(f.path()).unwrap();
    assert!(after_append > since);
    // Another lost acknowledgement must not recapture the now-larger position.
    f.run().await;
    assert_eq!(load_state(f.path()).unwrap().outbox[0].audit_since, since);
    assert_eq!(duduclaw_security::audit::audit_log_position(f.path()).unwrap(), after_append);
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = false;
    f.run().await;
    assert!(load_state(f.path()).unwrap().outbox.is_empty());
}

#[tokio::test]
async fn empty_log_position_is_not_mistaken_for_a_legacy_entry_on_retry() {
    let f = Fixture::standard().await;
    std::fs::write(f.path().join("security_audit.jsonl"), b"").unwrap();
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = true;
    f.run().await;
    assert_eq!(load_state(f.path()).unwrap().outbox[0].audit_since, 0);
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_audit_ack = false;
    f.run().await;
    let audit = duduclaw_security::audit::read_recent_events(f.path(), 100);
    assert_eq!(audit.iter().filter(|e| e.event_type == "skills_auto_approved").count(), 1);
}

#[tokio::test]
async fn outbox_recovery_processes_at_most_sixteen_entries_per_tick() {
    let f = Fixture::standard().await;
    let rec = f.record().await;
    let approved = evaluate(f.path(), &rec).await.unwrap();
    let template = Outbox::new(f.path(), &rec, &approved).unwrap();
    update_state(f.path(), |state| {
        for index in 0..=MAX_OUTBOX_PER_TICK {
            let mut entry = template.clone();
            entry.approval_id = format!("missing-{index}");
            state.outbox.push(entry);
        }
        Ok(())
    }).unwrap();
    f.run().await;
    let state = load_state(f.path()).unwrap();
    assert_eq!(state.outbox.len(), 1, "each missing approval consumes one recovery slot");
    assert_eq!(state.outbox[0].approval_id, format!("missing-{}", MAX_OUTBOX_PER_TICK));
    assert_eq!(f.record().await.status, ApprovalStatus::Pending, "new work waits for a free slot");
    f.run().await;
    assert!(load_state(f.path()).unwrap().outbox.is_empty());
    assert_eq!(f.record().await.status, ApprovalStatus::Approved);
}

#[tokio::test]
async fn new_outbox_deliveries_share_the_recovery_budget() {
    let f = Fixture::standard().await;
    let rec = f.record().await;
    let approved = evaluate(f.path(), &rec).await.unwrap();
    let template = Outbox::new(f.path(), &rec, &approved).unwrap();
    let other = f.broker.request("imported", expert_skills::SKILLS_ACTION_KIND,
        &rec.summary, rec.payload.clone(), 60).await.unwrap();
    update_state(f.path(), |state| {
        for index in 0..MAX_OUTBOX_PER_TICK - 1 {
            let mut entry = template.clone();
            entry.approval_id = format!("missing-{index}");
            state.outbox.push(entry);
        }
        Ok(())
    }).unwrap();
    f.run().await;
    let statuses = [f.record().await.status, f.broker.get(&other).await.unwrap().unwrap().status];
    assert_eq!(statuses.iter().filter(|s| **s == ApprovalStatus::Approved).count(), 1);
    assert_eq!(statuses.iter().filter(|s| **s == ApprovalStatus::Pending).count(), 1);
    assert_eq!(f.pushes().notices, 1);
    assert!(f.pushes().cards.is_empty(), "the excess candidate was deferred, not evaluated and refused");
    assert!(!load_state(f.path()).unwrap().evaluated.contains(other.as_str()));
}

#[tokio::test]
async fn outbox_effects_are_once_across_sweeps_and_lost_acknowledgements() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().notice_failures = 1;
    f.run().await;
    let state = load_state(f.path()).unwrap();
    assert_eq!(state.outbox.len(), 1);
    assert!(state.outbox[0].audit_done && state.outbox[0].activity_done);
    assert!(state.highest_versions.is_empty(), "not finalized while the notice is pending");
    // Simulate a crash after writing each idempotent sink but before its flag.
    update_state(f.path(), |state| {
        state.outbox[0].audit_done = false;
        state.outbox[0].activity_done = false;
        state.outbox[0].next_at = 0;
        Ok(())
    }).unwrap();
    f.run().await;
    f.run().await;
    assert_eq!(f.pushes().notices, 2, "one failure, one successful notice, no duplicate success");
    let audit = duduclaw_security::audit::read_recent_events(f.path(), 100);
    assert_eq!(audit.iter().filter(|e| e.event_type == "skills_auto_approved").count(), 1);
    let store = crate::task_store::TaskStore::open(f.path()).unwrap();
    let (rows, count) = store.list_activity(None, Some("skills_auto_approved"), 100, 0).await.unwrap();
    assert_eq!(count, 1);
    assert_eq!(rows[0].id, format!("skill-trust:{}", f.id));
    let state = load_state(f.path()).unwrap();
    assert!(state.outbox.is_empty());
    assert_eq!(state.highest_versions[0].manifest_sha256, hash(V120_MANIFEST.as_bytes()));
}

#[tokio::test]
async fn successful_outbox_audit_activity_and_notice_are_once_across_two_sweeps() {
    let f = Fixture::standard().await;
    f.run().await;
    f.run().await;
    assert_eq!(f.pushes().notices, 1);
    let audit = duduclaw_security::audit::read_recent_events(f.path(), 100);
    assert_eq!(audit.iter().filter(|e| e.event_type == "skills_auto_approved").count(), 1);
    let store = crate::task_store::TaskStore::open(f.path()).unwrap();
    assert_eq!(store.list_activity(None, Some("skills_auto_approved"), 100, 0).await.unwrap().1, 1);
}

#[tokio::test]
async fn failing_notice_retries_with_backoff_then_gives_up_and_retains_activity() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().notice_failures = usize::MAX;
    f.run().await;
    f.run().await;
    assert_eq!(f.pushes().notices, 1, "not due yet");
    for attempts in 2..=5 {
        update_state(f.path(), |s| { s.outbox[0].next_at = 0; Ok(()) }).unwrap();
        f.run().await;
        let state = load_state(f.path()).unwrap();
        assert_eq!(state.outbox[0].attempts, attempts);
        assert_eq!(state.outbox[0].notice_state, delivery::NoticeState::Pending);
        assert!(state.outbox[0].next_at > chrono::Utc::now().timestamp());
    }
    update_state(f.path(), |s| { s.outbox[0].next_at = 0; Ok(()) }).unwrap();
    let entry = load_state(f.path()).unwrap().outbox.remove(0);
    assert_eq!(process_outbox(f.path(), &f.broker, entry).await.unwrap(), Some(delivery::NoticeState::GaveUp));
    f.run().await;
    assert_eq!(f.pushes().notices, 6);
    let state = load_state(f.path()).unwrap();
    assert!(state.outbox.is_empty());
    assert_eq!(state.highest_versions.len(), 1);
    let store = crate::task_store::TaskStore::open(f.path()).unwrap();
    assert!(store.activity_exists(&format!("skill-trust:{}", f.id)).await.unwrap());
}

#[tokio::test]
async fn outbox_write_failure_never_reaches_policy_cas() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().fail_outbox_write = true;
    f.manual(INTERNAL).await;
    let state = load_state(f.path()).unwrap();
    assert!(state.outbox.is_empty());
    assert!(state.highest_versions.is_empty());
    assert!(state.evaluated.contains(f.id.as_str()));
}

#[tokio::test]
async fn human_wins_cas_drops_outbox_without_announcement_or_high_water() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().human_before_cas = true;
    f.run().await;
    f.run().await;
    assert_eq!(f.record().await.decided_by.as_deref(), Some("admin"));
    assert_eq!(f.pushes().notices, 0);
    assert!(f.pushes().cards.is_empty());
    let state = load_state(f.path()).unwrap();
    assert!(state.outbox.is_empty());
    assert!(state.highest_versions.is_empty());
    let audit = duduclaw_security::audit::read_recent_events(f.path(), 100);
    assert!(!audit.iter().any(|e| e.event_type == "skills_auto_approved"));
    let store = crate::task_store::TaskStore::open(f.path()).unwrap();
    assert!(!store.activity_exists(&format!("skill-trust:{}", f.id)).await.unwrap());
}

#[tokio::test]
async fn refused_card_retries_without_reevaluation_after_backoff() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().card_failures = 1;
    std::fs::write(f.path().join("config.toml"), "broken = [").unwrap();
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 1);
    assert!(f.record().await.notify_channel.is_none());
    let state = load_state(f.path()).unwrap();
    assert_eq!(state.cards[f.id.as_str()].card_attempts, 1);
    assert!(state.cards[f.id.as_str()].card_next_at > chrono::Utc::now().timestamp());
    // Make evaluation succeed now. The refusal must still stay manual.
    std::fs::write(f.path().join("config.toml"), pin_config("fixture", KEY_A, &minisign_key_id(KEY_A).unwrap())).unwrap();
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 1);
    update_state(f.path(), |s| { s.cards.get_mut(f.id.as_str()).unwrap().card_next_at = 0; Ok(()) }).unwrap();
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 2);
    assert!(f.pushes().cards[1].summary.contains(CONFIG));
    assert!(f.record().await.notify_channel.is_some());
    assert_eq!(f.record().await.status, ApprovalStatus::Pending);
    update_state(f.path(), |s| { s.cards.get_mut(f.id.as_str()).unwrap().card_next_at = 0; Ok(()) }).unwrap();
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 2, "delivered cards stop retrying");
}

#[tokio::test]
async fn refused_card_retry_stops_and_prunes_after_human_decision() {
    let f = Fixture::standard().await;
    hooks().lock().unwrap().get_mut(f.path()).unwrap().card_failures = usize::MAX;
    std::fs::write(f.path().join("config.toml"), "broken = [").unwrap();
    f.run().await;
    assert!(f.record().await.notify_channel.is_none());
    update_state(f.path(), |s| { s.cards.get_mut(f.id.as_str()).unwrap().card_next_at = 0; Ok(()) }).unwrap();
    f.broker.decide(&f.id, false, "admin").await.unwrap();
    f.run().await;
    assert_eq!(f.pushes().cards.len(), 1);
    let state = load_state(f.path()).unwrap();
    assert!(state.cards.is_empty() && state.evaluated.is_empty());
}
