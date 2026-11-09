use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    LiveRequestVerifier, Principal, PublicOrigin}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy::{authorize, ResourceResolver, ToolPolicy}};
use serde_json::{json, Value};

const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);

struct Live;
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
struct Fixed(Vec<ResourceGrant>);
impl ResourceResolver for Fixed {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}
fn entry(name: &str, offset: u64, length: u64) -> ArchiveEntry {
    ArchiveEntry { name: Text::new(name.into()).unwrap(), directory: false, offset, length }
}
fn call(origins: &[&str]) -> AuthorizedToolCall {
    let grant = ResourceGrant::PublicOrigins { origins: List::new(origins.iter()
        .map(|origin| if *origin == "*" { PublicOrigin::any() } else { PublicOrigin::parse(origin).unwrap() }).collect()).unwrap() };
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("skill_hub_install".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::EmployeeScope { id: EMPLOYEE }, ResourceGrant::Employee { id: EMPLOYEE },
            grant]).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, std::sync::Arc::new(Live));
    authorize(&context, "skill_hub_install", &json!({"hub":"clawhub","skill_name":"notes"}),
        &Fixed(vec![ResourceGrant::Employee { id: EMPLOYEE }])).unwrap()
}

#[test]
fn h12b_archive_url_is_host_selected_for_reviewed_hubs_only() {
    assert_eq!(archive_url("clawhub", "notes").as_deref(), Some("https://clawhub.ai/api/v1/download?slug=notes"));
    for hub in ["anthropic-skills", "github", "lobehub", "skills-sh", "clawhub2", ""] {
        assert!(archive_url(hub, "notes").is_none(), "{hub}");
    }
    for name in ["", "../x", "a&owner=evil", "a b", &"x".repeat(65)] {
        assert!(archive_url("clawhub", name).is_none(), "{name}");
    }
}

#[test]
fn h13c_detail_url_same_origin_as_archive() {
    assert_eq!(detail_url("clawhub", "notes").as_deref(), Some("https://clawhub.ai/api/v1/skills/notes"));
    for (hub, name) in [("clawhub", "notes"), ("clawhub", "../x"), ("clawhub", "a&owner=evil"), ("github", "notes"), ("", "notes")] {
        assert_eq!(detail_url(hub, name).is_some(), archive_url(hub, name).is_some(), "{hub} {name}");
        if let (Some(detail), Some(archive)) = (detail_url(hub, name), archive_url(hub, name)) {
            let origin = |url: &str| reqwest::Url::parse(url).unwrap().origin();
            assert_eq!(origin(&detail), origin(&archive));
        }
    }
}

/// Captured-shape ClawHub `/api/v1/skills/<slug>` responses.
fn detail(skill: Value, moderation: Option<Value>) -> Vec<u8> {
    let mut body = json!({"skill": skill});
    if let Some(moderation) = moderation { body["moderation"] = moderation; }
    serde_json::to_vec(&body).unwrap()
}

#[test]
fn h13c_hub_source_verdict_from_fixture_responses() {
    let clean = detail(json!({"slug":"notes","displayName":"Notes","isSuspicious":false,"description":"---\nname: notes\n---\n"}),
        Some(json!({"status":"clean"})));
    assert_eq!(source_verdict(Ok(clean.as_slice()), "notes"), Ok(Some("clean".into())));
    // No moderation data: advisory (None), like the unprotected path.
    assert_eq!(source_verdict(Ok(detail(json!({"slug":"notes"}), None).as_slice()), "notes"), Ok(None));
    assert_eq!(source_verdict(Ok(detail(json!({"slug":"notes"}), Some(json!({"verdict":"pending-review"}))).as_slice()),
        "notes"), Ok(Some("unknown".into())));
    // The hub's own suspicious / moderation flags refuse (layer 1, fail closed).
    for blocked in [detail(json!({"slug":"notes","isSuspicious":true}), Some(json!({"status":"clean"}))),
        detail(json!({"slug":"notes","isSuspicious":"yes"}), None),
        detail(json!({"slug":"notes"}), Some(json!({"status":"flagged"}))),
        detail(json!({"slug":"notes"}), Some(json!({"verdict":"malicious"}))),
        detail(json!({"slug":"notes","isSuspicious":false}), Some(json!({"status":"blocked"})))] {
        let error = source_verdict(Ok(blocked.as_slice()), "notes").unwrap_err();
        assert!(error.contains("可疑或惡意"), "{error}");
    }
    // Ambiguous slug (409 AMBIGUOUS_SKILL_SLUG): refused, never resolved.
    assert!(source_verdict(Err(409), "notes").unwrap_err().contains("多位發布者"));
    // Not found, other statuses, no reply: refused.
    for status in [404, 403, 500, 0] { assert!(source_verdict(Err(status), "notes").is_err(), "{status}"); }
    // Unreadable, unexpected or foreign bodies: refused.
    let oversized = vec![b' '; MAX_DETAIL_BYTES + 1];
    for body in [b"not json".to_vec(), b"{}".to_vec(), serde_json::to_vec(&json!({"skill":"notes"})).unwrap(),
        detail(json!({"slug":"other"}), Some(json!({"status":"clean"}))), detail(json!({"slug":7}), None), oversized] {
        assert!(source_verdict(Ok(body.as_slice()), "notes").is_err());
    }
}

#[test]
fn h13c_owned_scan_refuses_high_and_critical() {
    assert_eq!(scan_admits(true, "Low"), Ok(()));
    assert_eq!(scan_admits(true, "Medium"), Ok(()));
    for (passed, risk) in [(true, "High"), (true, "Critical"), (false, "Low")] {
        assert!(scan_admits(passed, risk).is_err(), "{passed} {risk}");
    }
}

#[tokio::test]
async fn h13c_install_refuses_blocking_source_verdict() {
    let home = tempfile::tempdir().unwrap();
    let gated = GatedManifest { hub: "clawhub".into(), skill_name: "notes".into(), manifest_name: "notes".into(),
        content: "---\nname: notes\n---\nx".into(), publisher: None, signature: None,
        signature_status: duduclaw_core::publishers::SignatureStatus::unsigned(None), risk_level: "Low".into(),
        findings: 0, trust_floor: duduclaw_agent::trust_tier::TrustTier::Active, source_verdict: Some("suspicious".into()) };
    assert!(install_owned(home.path(), &call(&["*"]), "main", &gated).await.is_err());
    assert!(!home.path().join("agents").exists(), "nothing written");
}

#[test]
fn h12b_archive_hostile_entries_refused_on_host() {
    let skill = b"---\nname: notes\n---\n# Notes";
    let len = skill.len() as u64;
    // Accepted: root SKILL.md, or SKILL.md in the only top-level folder.
    assert_eq!(select_skill_document(&[entry("SKILL.md", 0, len)], skill).unwrap(), skill);
    let mut bytes = skill.to_vec(); bytes.extend_from_slice(b"extra");
    assert_eq!(select_skill_document(&[entry("notes/SKILL.md", 0, len), entry("notes/ref.md", len, 5)], &bytes).unwrap(), skill);
    // The worker's manifest is untrusted: traversal, absolute, backslash,
    // drive/ADS colon and control-character names refuse even if E let them by.
    for name in ["../SKILL.md", "/SKILL.md", "notes/../SKILL.md", "notes\\SKILL.md", "C:SKILL.md", "notes//SKILL.md",
        "./SKILL.md", "notes/\u{0}SKILL.md", "notes/\u{85}SKILL.md"] {
        assert!(select_skill_document(&[entry(name, 0, len)], skill).is_err(), "{name:?}");
    }
    // Ranges must tile the expanded bytes exactly (no overlap, gap or overrun).
    assert!(select_skill_document(&[entry("SKILL.md", 1, len - 1)], skill).is_err());
    assert!(select_skill_document(&[entry("SKILL.md", 0, len + 1)], skill).is_err());
    assert!(select_skill_document(&[entry("SKILL.md", 0, len - 1)], skill).is_err());
    assert!(select_skill_document(&[entry("a", 0, 4), entry("SKILL.md", 2, len - 2)], skill).is_err());
    let mut directory = entry("notes", 0, 1); directory.directory = true;
    assert!(select_skill_document(&[directory, entry("SKILL.md", 1, len - 1)], skill).is_err());
    // Exactly one document, at most one folder deep, in the only top-level folder.
    assert!(select_skill_document(&[entry("README.md", 0, len)], skill).is_err(), "no SKILL.md");
    let mut twice = skill.to_vec(); twice.extend_from_slice(skill);
    assert!(select_skill_document(&[entry("SKILL.md", 0, len), entry("b/SKILL.md", len, len)], &twice).is_err());
    assert!(select_skill_document(&[entry("a/b/SKILL.md", 0, len)], skill).is_err(), "too deep");
    assert!(select_skill_document(&[entry("a/SKILL.md", 0, len), entry("b/x.md", len, 5)], &bytes).is_err(), "two folders");
    // Size and entry-count ceilings.
    let big = vec![b'a'; (MAX_SKILL_BYTES + 1) as usize];
    assert!(select_skill_document(&[entry("SKILL.md", 0, big.len() as u64)], &big).is_err());
    assert!(select_skill_document(&[entry("SKILL.md", 0, 0)], b"").is_err());
    let many: Vec<_> = (0..=MAX_ARCHIVE_ENTRIES as u64).map(|i| entry(&format!("f{i}.md"), i, 1)).collect();
    assert!(select_skill_document(&many, &vec![b'x'; many.len()]).is_err());
    assert!(select_skill_document(&[], b"").is_err());
}

#[tokio::test]
async fn h12b_fetch_refused_before_network_without_matching_grant_or_hub() {
    let home = tempfile::tempdir().unwrap();
    // The granted public origins do not include the host-selected hub origin.
    let error = fetch_owned(home.path(), &call(&["https://example.com"]), "main").await.unwrap_err();
    assert!(error.contains("公開網路授權"), "{error}");
    // A hub the admin did not enable is refused before any fetch.
    std::fs::write(home.path().join("config.toml"), "[skill_hubs]\nenabled = [\"anthropic-skills\"]\n").unwrap();
    let error = fetch_owned(home.path(), &call(&["*"]), "main").await.unwrap_err();
    assert!(error.contains("未設定"), "{error}");
    // The bound employee label must be a real employee id.
    for agent in ["", "../x", "*"] {
        assert!(fetch_owned(home.path(), &call(&["*"]), agent).await.is_err(), "{agent}");
    }
    assert!(!home.path().join("agents").exists(), "nothing written");
}

#[tokio::test]
async fn h12b_install_refuses_a_manifest_not_from_this_call() {
    let home = tempfile::tempdir().unwrap();
    let gated = GatedManifest { hub: "clawhub".into(), skill_name: "other".into(), manifest_name: "other".into(),
        content: "---\nname: other\n---\nx".into(), publisher: None, signature: None,
        signature_status: duduclaw_core::publishers::SignatureStatus::unsigned(None), risk_level: "Low".into(),
        findings: 0, trust_floor: duduclaw_agent::trust_tier::TrustTier::Active, source_verdict: None };
    assert!(install_owned(home.path(), &call(&["*"]), "main", &gated).await.is_err());
    let mut signed = gated.clone();
    signed.skill_name = "notes".into(); signed.manifest_name = "notes".into(); signed.publisher = Some("fixture".into());
    assert!(install_owned(home.path(), &call(&["*"]), "main", &signed).await.is_err());
    assert!(!home.path().join("agents").exists(), "nothing written");
}
