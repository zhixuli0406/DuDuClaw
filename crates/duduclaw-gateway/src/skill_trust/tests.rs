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
struct Pushes { cards: Vec<ApprovalRecord>, notices: usize }
fn hooks() -> &'static Mutex<BTreeMap<PathBuf, Pushes>> {
    static HOOKS: OnceLock<Mutex<BTreeMap<PathBuf, Pushes>>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
pub(crate) fn capture_card(home: Option<&Path>, rec: &ApprovalRecord) -> bool {
    let Some(home) = home else { return false; };
    let mut hooks = hooks().lock().unwrap();
    let Some(pushes) = hooks.get_mut(home) else { return false; };
    pushes.cards.push(rec.clone());
    true
}
pub(super) fn capture_notice(home: &Path) -> bool {
    let mut hooks = hooks().lock().unwrap();
    let Some(pushes) = hooks.get_mut(home) else { return false; };
    pushes.notices += 1;
    true
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
        let quarantine = expert_skills::quarantined_skill_dir(h, "imported", "alice", "helper");
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::write(quarantine.join("SKILL.md"), "# Regenerated skill\nHelp with daily notes.\n").unwrap();
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
        s.highest_versions.push(HighestVersion { publisher: "fixture".into(), pack: "fixture-pack".into(), version: "1.2.0".into() }); Ok(())
    }).unwrap();
    f.manual(VERSION).await;
    let f = Fixture::standard().await;
    update_state(f.path(), |s| {
        s.highest_versions.push(HighestVersion { publisher: "fixture".into(), pack: "fixture-pack".into(), version: "1.2.0".into() }); Ok(())
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
async fn high_risk_scan_goes_manual() {
    let f = Fixture::standard().await;
    std::fs::write(f.quarantine().join("SKILL.md"), "Use key sk-ant-abc123 to connect").unwrap();
    f.manual(SCAN).await;
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
    std::fs::write(f.quarantine().join("SKILL.md"), "Different safe text").unwrap();
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
