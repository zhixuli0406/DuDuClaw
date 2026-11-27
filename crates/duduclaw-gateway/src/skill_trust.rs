//! Gateway authorization of imported skills by an explicitly pinned in-house key.
//! CLI evidence is input, never a verdict. Every refusal leaves the same approval
//! pending and hands it back to the normal human decision path.

use std::{cmp::Ordering, collections::{BTreeMap, BTreeSet}, io::Write, path::Path};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use duduclaw_core::publishers::{TrustStore, validate_pubkey, validate_publisher_id, verify_minisig};
use duduclaw_security::audit::{append_audit_event, AuditEvent, Severity};
use crate::{approval::{ApprovalBroker, ApprovalRecord, ApprovalStatus}, expert_skills};
use crate::skill_lifecycle::security_scanner::{scan_skill, RiskLevel};

mod files;
mod binding;
mod delivery;
use delivery::{CardRetry, Outbox, process_outbox, retry_card};
use files::{Directory, safe_relative, segment};

const STATE_FILE: &str = "skill_trust_state.json";
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVALUATED: usize = 4096;
const MAX_OUTBOX_PER_TICK: usize = 16;
const INTERNAL: &str = "自動放行檢查暫時無法完成，請人工確認";
const EVIDENCE: &str = "來源證據缺漏、無法讀取或含不安全的檔案";
const CONFIG: &str = "無法讀取自家技能來源設定";
const KEY: &str = "簽章金鑰不在自家來源清單";
const SIGNATURE: &str = "簽章驗證失敗，需人工確認";
const IDENTITY: &str = "來源證據與簽章清單不符";
const FILES: &str = "隔離檔案與來源清單不符";
const HASH: &str = "腳本或附件內容與簽章不符";
const DESCRIPTION: &str = "技能說明與簽章內容不符";
const BINARY: &str = "含二進位檔，需人工確認";
const SCAN: &str = "安全掃描判定風險過高";
const APPROVAL: &str = "技能內容與待核准紀錄不符";
const VERSION: &str = "版本格式無法確認或低於先前已放行版本";

/// Canonical minisign key id: the little-endian u64 printed as 16 uppercase hex digits.
pub fn minisign_key_id(key: &str) -> Result<String, String> {
    validate_pubkey(key).map_err(|_| KEY.to_owned())?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(key).map_err(|_| KEY)?;
    let id: [u8; 8] = decoded.get(2..10).ok_or(KEY)?.try_into().map_err(|_| KEY)?;
    Ok(format!("{:016X}", u64::from_le_bytes(id)))
}

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    skill_trust: TrustConfig,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustConfig {
    #[serde(default)]
    auto_approve: Vec<PinnedKey>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedKey {
    publisher: String,
    key: String,
    key_id: String,
    added_by: String,
    added_at: String,
}
impl PinnedKey {
    fn usable(&self) -> bool {
        self.publisher != "duduclaw" && validate_publisher_id(&self.publisher).is_ok()
            && !self.added_by.is_empty() && !self.added_at.is_empty()
            && minisign_key_id(&self.key).is_ok_and(|id| id == self.key_id)
    }
}
fn read_config(root: &Directory) -> Result<TrustConfig, String> {
    let raw = root.read("config.toml", 2 * 1024 * 1024).map_err(|_| CONFIG)?;
    let text = std::str::from_utf8(&raw).map_err(|_| CONFIG)?;
    toml::from_str::<Config>(text).map(|c| c.skill_trust).map_err(|_| CONFIG.into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    version: u32,
    publisher: String,
    pack: String,
    pack_version: String,
    skills: Vec<SkillProvenance>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillProvenance {
    agent: String,
    name: String,
    source_dir: String,
    files: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Entry { agent: String, name: String, digest: String }

struct Manifest {
    pack: String,
    version: String,
    publisher: String,
    files: BTreeMap<String, String>,
}
fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn digest_valid(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    // Same three headers and strict sorted, unique lowercase hash/path rules
    // as core::pack::signatures::verify_inner; no publisher-wide verification.
    let text = std::str::from_utf8(bytes).map_err(|_| IDENTITY)?;
    let mut lines = text.lines();
    let mut header = |prefix| lines.next().and_then(|s| s.strip_prefix(prefix))
        .filter(|s| !s.is_empty()).map(str::to_owned).ok_or(IDENTITY);
    let pack = header("name: ")?;
    let version = header("version: ")?;
    let publisher = header("publisher: ")?;
    validate_publisher_id(&publisher).map_err(|_| IDENTITY)?;
    let mut files = BTreeMap::new();
    let mut last = String::new();
    for line in lines {
        let (digest, path) = line.split_once("  ").ok_or(IDENTITY)?;
        if !digest_valid(digest) || !safe_relative(path) || path <= last.as_str()
            || path == "MANIFEST.sha256" || path == "MANIFEST.sha256.minisig"
        { return Err(IDENTITY.into()); }
        last = path.to_owned();
        files.insert(path.to_owned(), digest.to_owned());
    }
    Ok(Manifest { pack, version, publisher, files })
}

/// Facts verified against the quarantined bytes, bound to the existing approval.
#[derive(Debug, Clone, Serialize)]
pub struct AutoApproval {
    pub publisher: String,
    pub key_id: String,
    pub pack: String,
    pub pack_version: String,
    pub manifest_sha256: String,
    pub skills: Vec<(String, String, String)>,
}

fn candidate(rec: &ApprovalRecord) -> bool {
    rec.action_kind == expert_skills::SKILLS_ACTION_KIND
        && rec.status == ApprovalStatus::Pending
        && rec.payload.get("auto_approval_candidate").and_then(serde_json::Value::as_bool) == Some(true)
}

/// Reasons are plain zh-TW constants, never IO error strings or file paths.
pub async fn evaluate(home: &Path, rec: &ApprovalRecord) -> Result<AutoApproval, String> {
    if !candidate(rec) || rec.is_stale(chrono::Utc::now()) { return Err(APPROVAL.into()); }
    let slug = rec.payload.get("slug").and_then(serde_json::Value::as_str).ok_or(APPROVAL)?;
    if slug != rec.agent_id || !segment(slug) { return Err(APPROVAL.into()); }
    let root = Directory::open(home).map_err(|_| EVIDENCE)?;
    let expert = root.descend(&format!("experts/{slug}")).map_err(|_| EVIDENCE)?;
    let evidence = expert.child("provenance").map_err(|_| EVIDENCE)?;
    let provenance: Provenance = serde_json::from_slice(&evidence.read("provenance.json", 1024 * 1024)
        .map_err(|_| EVIDENCE)?).map_err(|_| EVIDENCE)?;
    let manifest_bytes = evidence.read("MANIFEST.sha256", 2 * 1024 * 1024).map_err(|_| EVIDENCE)?;
    let signature_bytes = evidence.read("MANIFEST.sha256.minisig", 4096).map_err(|_| EVIDENCE)?;
    let signature = std::str::from_utf8(&signature_bytes).map_err(|_| SIGNATURE)?;
    let manifest = parse_manifest(&manifest_bytes)?;
    if provenance.version != 1 || manifest.publisher != provenance.publisher { return Err(IDENTITY.into()); }
    let config = read_config(&root)?;
    // TrustStore::load uses path-based read_to_string. Use its SAME parser on
    // the no-follow snapshot instead: reopening by path would follow a link
    // swapped in after validation. Missing stores also deny (no matching key).
    let store_bytes = root.read(duduclaw_core::publishers::TRUST_STORE_FILE, 2 * 1024 * 1024).map_err(|_| KEY)?;
    let store = TrustStore::parse(std::str::from_utf8(&store_bytes).map_err(|_| KEY)?).map_err(|_| KEY)?;
    let pins: Vec<_> = config.auto_approve.iter().filter(|p| p.usable()
        && p.publisher == manifest.publisher
        && store.keys(&p.publisher).contains(&p.key.as_str())).collect();
    if pins.is_empty() { return Err(KEY.into()); }
    let pin = pins.into_iter().find(|p| verify_minisig(&manifest_bytes, signature, &p.key).is_ok())
        .ok_or(SIGNATURE)?;
    if manifest.pack != provenance.pack || manifest.version != provenance.pack_version { return Err(IDENTITY.into()); }
    let entries: Vec<Entry> = serde_json::from_value(rec.payload.get("entries").cloned().ok_or(APPROVAL)?)
        .map_err(|_| APPROVAL)?;
    let mut identities = BTreeSet::new();
    if entries.is_empty() || entries.len() > 1024 || entries.iter().any(|e|
        !segment(&e.agent) || !segment(&e.name) || !digest_valid(&e.digest)
        || !duduclaw_core::is_valid_agent_id(&e.agent)
        || !duduclaw_agent::skill_loader::is_safe_skill_name(&e.name)
        || !identities.insert((e.agent.clone(), e.name.clone())))
    { return Err(APPROVAL.into()); }
    // Keep the exact bytes scanned and hashed, rather than re-reading between
    // the signature check, scanner and tree digest binding.
    let mut snapshots = Vec::new();
    let mut total = 0usize;
    for entry in &entries {
        let matching: Vec<_> = provenance.skills.iter().filter(|s| s.agent == entry.agent && s.name == entry.name).collect();
        if matching.len() != 1 { return Err(FILES.into()); }
        let source = matching[0];
        if !safe_relative(&source.source_dir) || source.files.is_empty()
            || source.files.iter().any(|p| !safe_relative(p))
            || source.files.windows(2).any(|p| p[0] >= p[1])
            || !source.files.iter().any(|p| p == "SKILL.md")
        { return Err(FILES.into()); }
        let dir = expert.descend(&format!("skills-disabled/{}/{}", entry.agent, entry.name)).map_err(|_| FILES)?;
        let actual = files::snapshot(&dir).map_err(|_| FILES)?;
        if actual.keys().ne(source.files.iter()) { return Err(FILES.into()); }
        for (path, bytes) in &actual {
            total = total.checked_add(bytes.len()).ok_or(FILES)?;
            if total > 64 * 1024 * 1024 { return Err(FILES.into()); }
            let signed_hash = manifest.files.get(&format!("{}/{path}", source.source_dir)).ok_or(FILES)?;
            if path == "SKILL.md" {
                let signed_dir = evidence.descend(&format!("sources/{}/{}", entry.agent, entry.name)).map_err(|_| EVIDENCE)?;
                let signed = signed_dir.read("SKILL.md", 512 * 1024).map_err(|_| EVIDENCE)?;
                if *signed_hash != hash(&signed) { return Err(HASH.into()); }
                binding::verify(bytes, &signed, &entry.name, &manifest.version)?;
            } else if *signed_hash != hash(bytes) { return Err(HASH.into()); }
        }
        snapshots.push(actual);
    }
    for actual in &snapshots {
        for bytes in actual.values() {
            let text = std::str::from_utf8(bytes).map_err(|_| BINARY)?;
            // NUL-bearing data is binary even when technically UTF-8.
            if text.contains('\0') { return Err(BINARY.into()); }
            if scan_skill(text, None).risk_level >= RiskLevel::High { return Err(SCAN.into()); }
        }
    }
    let state_bytes = expert.read("skills-state.json", 2 * 1024 * 1024).map_err(|_| APPROVAL)?;
    // Same type/parser as read_skills_state, without its path-based re-open.
    let state: expert_skills::SkillsState = serde_json::from_slice(&state_bytes).map_err(|_| APPROVAL)?;
    {
        if state.status != expert_skills::SkillsStatus::PendingApproval || state.approval_id.as_deref() != Some(rec.id.as_str()) {
            return Err(APPROVAL.into());
        }
        let mut recorded: Vec<_> = state.skills.iter().map(|s| Entry { agent: s.agent.clone(), name: s.name.clone(), digest: s.digest.clone() }).collect();
        let mut requested = entries.clone();
        recorded.sort(); requested.sort();
        if recorded != requested { return Err(APPROVAL.into()); }
    }
    for (entry, snapshot) in entries.iter().zip(&snapshots) {
        if snapshot_digest(snapshot) != entry.digest { return Err(APPROVAL.into()); }
    }
    let result = AutoApproval {
        publisher: manifest.publisher, key_id: pin.key_id.clone(), pack: manifest.pack,
        pack_version: manifest.version, manifest_sha256: hash(&manifest_bytes),
        skills: entries.into_iter().map(|e| (e.agent, e.name, e.digest)).collect(),
    };
    check_version(&load_state(home)?, &result)?;
    Ok(result)
}

fn snapshot_digest(snapshot: &BTreeMap<String, Vec<u8>>) -> String {
    // Exact tree_digest encoding, over the very bytes verified and scanned.
    // promote_all still checks the final copied tree with tree_digest.
    let mut digest = Sha256::new();
    for (path, bytes) in snapshot {
        digest.update((path.len() as u64).to_le_bytes()); digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes()); digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

#[derive(Debug, PartialEq, Eq)]
struct Version { numbers: Vec<u64>, pre: Option<Vec<String>> }
impl Version {
    fn parse(value: &str) -> Result<Self, String> {
        if value.len() > 256 { return Err(VERSION.into()); }
        let (main, pre) = value.split_once('-').map_or((value, None), |(a,b)| (a, Some(b)));
        let numbers = main.split('.').map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0')) {
                return Err(VERSION.to_owned());
            }
            part.parse::<u64>().map_err(|_| VERSION.to_owned())
        }).collect::<Result<Vec<_>, _>>()?;
        let pre = pre.map(|pre| pre.split('.').map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || (part.bytes().all(|b| b.is_ascii_digit()) && part.len() > 1 && part.starts_with('0'))
            { Err(VERSION.to_owned()) } else { Ok(part.to_owned()) }
        }).collect::<Result<Vec<_>, _>>()).transpose()?;
        Ok(Self { numbers, pre })
    }
    fn compare(&self, other: &Self) -> Ordering {
        for n in 0..self.numbers.len().max(other.numbers.len()) {
            let order = self.numbers.get(n).unwrap_or(&0).cmp(other.numbers.get(n).unwrap_or(&0));
            if order != Ordering::Equal { return order; }
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => {
                for (a, b) in a.iter().zip(b) {
                    let numeric = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
                    let order = match (numeric(a), numeric(b)) {
                        (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => a.cmp(b),
                    };
                    if order != Ordering::Equal { return order; }
                }
                a.len().cmp(&b.len())
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    highest_versions: Vec<HighestVersion>,
    evaluated: BTreeSet<String>,
    #[serde(default)]
    outbox: Vec<Outbox>,
    #[serde(default)]
    cards: BTreeMap<String, CardRetry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HighestVersion {
    publisher: String, pack: String, version: String,
    // Legacy high-water entries have no digest: equal versions fail closed.
    #[serde(default)]
    manifest_sha256: String,
}
impl From<&AutoApproval> for HighestVersion {
    fn from(approved: &AutoApproval) -> Self {
        Self { publisher: approved.publisher.clone(), pack: approved.pack.clone(),
            version: approved.pack_version.clone(), manifest_sha256: approved.manifest_sha256.clone() }
    }
}
fn load_state(home: &Path) -> Result<State, String> {
    let root = Directory::open(home).map_err(|_| INTERNAL)?;
    let bytes = match root.read(STATE_FILE, MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(_) => return Err(INTERNAL.into()),
    };
    let state: State = serde_json::from_slice(&bytes).map_err(|_| INTERNAL)?;
    let mut keys = BTreeSet::new();
    if state.evaluated.len() > MAX_EVALUATED || state.highest_versions.len() > 4096
        || state.outbox.len() > MAX_EVALUATED || state.cards.len() > MAX_EVALUATED
        || state.highest_versions.iter().any(|v| Version::parse(&v.version).is_err()
            || validate_publisher_id(&v.publisher).is_err() || v.pack.is_empty()
            || (!v.manifest_sha256.is_empty() && !digest_valid(&v.manifest_sha256))
            || !keys.insert((&v.publisher, &v.pack)))
    { return Err(INTERNAL.into()); }
    let mut ids = BTreeSet::new();
    if state.outbox.iter().any(|o| !ids.insert(&o.approval_id) || o.approval_id.is_empty()
        || !segment(&o.slug) || validate_publisher_id(&o.publisher).is_err()
        || o.pack.is_empty() || Version::parse(&o.version).is_err() || !digest_valid(&o.manifest_sha256)
        || o.key_id.len() != 16 || !o.key_id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
        || o.skills.is_empty() || o.skills.len() > 1024 || o.skills.iter().any(|(agent, name, digest)|
            !segment(agent) || !segment(name) || !digest_valid(digest)))
    { return Err(INTERNAL.into()); }
    Ok(state)
}
fn check_version(state: &State, approved: &AutoApproval) -> Result<(), String> {
    let version = Version::parse(&approved.pack_version)?;
    for old in &state.highest_versions {
        if old.publisher == approved.publisher && old.pack == approved.pack {
            let order = version.compare(&Version::parse(&old.version)?);
            if order == Ordering::Less || (order == Ordering::Equal && old.manifest_sha256 != approved.manifest_sha256) {
                return Err(VERSION.into());
            }
        }
    }
    Ok(())
}
fn update_state<T>(home: &Path, change: impl FnOnce(&mut State) -> Result<T, String>) -> Result<T, String> {
    let _root = Directory::open(home).map_err(|_| INTERNAL)?;
    // with_file_lock uses a sidecar; reject pre-existing link/special locks.
    let lock = home.join(format!("{STATE_FILE}.lock"));
    match std::fs::symlink_metadata(&lock) {
        Ok(m) if duduclaw_core::fs_safety::is_link_metadata(&m) || !m.is_file() => return Err(INTERNAL.into()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(INTERNAL.into()),
        _ => {}
    }
    duduclaw_core::with_file_lock(&home.join(STATE_FILE), || {
        let mut state = load_state(home).map_err(std::io::Error::other)?;
        let result = change(&mut state).map_err(std::io::Error::other)?;
        #[cfg(test)]
        if tests::fail_outbox_write(home, &state) { return Err(std::io::Error::other(INTERNAL)); }
        let bytes = serde_json::to_vec(&state)?;
        if bytes.len() as u64 > MAX_STATE_BYTES { return Err(std::io::Error::other(INTERNAL)); }
        let mut tmp = tempfile::NamedTempFile::new_in(home)?;
        tmp.write_all(&bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(home.join(STATE_FILE)).map_err(|e| e.error)?;
        #[cfg(unix)]
        _root.sync()?;
        Ok(result)
    }).map_err(|_| INTERNAL.into())
}

fn reason_code(reason: &str) -> &'static str {
    match reason {
        EVIDENCE => "evidence_unreadable", CONFIG => "config_unreadable", KEY => "key_not_pinned",
        SIGNATURE => "signature_invalid", IDENTITY => "manifest_identity", FILES => "file_set_mismatch",
        HASH => "file_hash_mismatch", DESCRIPTION => "skill_description_mismatch", BINARY => "binary_file", SCAN => "scan_high_risk",
        APPROVAL => "approval_mismatch", VERSION => "version_rollback_or_invalid", _ => "internal_error",
    }
}
async fn refuse(home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord, reason: &str) {
    append_audit_event(home, &AuditEvent::new("skills_auto_approval_refused", &rec.agent_id, Severity::Warning,
        serde_json::json!({"approval_id": rec.id.as_str(), "reason_code": reason_code(reason), "reason": reason})));
    // Only the delivered copy changes; stored summary and payload stay intact.
    if let Ok(Some(mut current)) = broker.get(&rec.id).await {
        if current.status == ApprovalStatus::Pending {
            current.summary.push_str(&format!("（未自動放行：{reason}）"));
            broker.push_new_request(&current).await;
        }
    }
}

/// Evaluate each candidate once. A process without the gateway lock does no IO.
pub async fn run_once(home: &Path, broker: &ApprovalBroker) {
    if !duduclaw_core::gateway_instance::held(home) || broker.home_dir().as_deref() != Some(home) { return; }
    // Serializes overlapping ticks/callers without holding a filesystem lock
    // across awaits. The cross-process state mutation still uses with_file_lock.
    static RUN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _running = RUN.lock().await;
    if !duduclaw_core::gateway_instance::held(home) { return; }
    let mut outbox_budget = MAX_OUTBOX_PER_TICK;
    // Recover committed side effects even when there are no pending approvals.
    if let Ok(state) = load_state(home) {
        let now = chrono::Utc::now().timestamp();
        for entry in state.outbox.into_iter().filter(|entry|
            !entry.audit_done || !entry.activity_done || entry.next_at <= now
                || entry.notice_state != delivery::NoticeState::Pending).take(outbox_budget)
        {
            outbox_budget -= 1;
            if let Err(error) = process_outbox(home, broker, entry).await {
                tracing::warn!(%error, "skill trust outbox remains pending");
            }
        }
    }
    let records = match broker.list_pending(None).await { Ok(rows) => rows, Err(_) => return };
    let pending: BTreeSet<_> = records.iter().filter(|r| candidate(r)).map(|r| r.id.to_string()).collect();
    let seen = update_state(home, |state| {
        state.evaluated.retain(|id| pending.contains(id));
        state.cards.retain(|id, _| pending.contains(id));
        Ok(state.evaluated.clone())
    });
    for rec in records.into_iter().filter(candidate) {
        if !duduclaw_core::gateway_instance::held(home) { return; }
        let seen = match &seen {
            Ok(seen) => seen,
            Err(_) => { refuse(home, broker, &rec, INTERNAL).await; continue; }
        };
        if seen.contains(rec.id.as_str()) {
            // Legacy evaluated entries had no delivery state. Never re-evaluate.
            let _ = update_state(home, |state| {
                if !state.outbox.iter().any(|o| o.approval_id == rec.id.as_str()) {
                    state.cards.entry(rec.id.to_string()).or_insert_with(|| CardRetry::new(INTERNAL));
                }
                Ok(())
            });
            retry_card(home, broker, &rec).await;
            continue;
        }
        // New approvals share the recovery budget; leave excess candidates
        // pending until a later tick rather than creating unbounded work.
        if outbox_budget == 0 { break; }
        let evaluated = evaluate(home, &rec).await.and_then(|approved| {
            let entry = Outbox::new(home, &rec, &approved)?;
            Ok((approved, entry))
        });
        let reserved = update_state(home, |state| {
            if state.evaluated.contains(rec.id.as_str()) { return Ok(false); }
            if state.evaluated.len() >= MAX_EVALUATED { return Err(INTERNAL.into()); }
            if let Ok((approved, entry)) = &evaluated {
                check_version(state, approved)?;
                // An unresolved outbox reserves its version/digest without
                // changing the high-water mark. Identical replays still pass.
                for other in &state.outbox {
                    if other.publisher == approved.publisher && other.pack == approved.pack {
                        let order = Version::parse(&approved.pack_version)?.compare(&Version::parse(&other.version)?);
                        if order == Ordering::Less || (order == Ordering::Equal && other.manifest_sha256 != approved.manifest_sha256) {
                            return Err(VERSION.into());
                        }
                    }
                }
                if state.outbox.len() >= MAX_EVALUATED
                    || (state.highest_versions.len() >= 4096 && !state.highest_versions.iter().any(|v|
                        v.publisher == approved.publisher && v.pack == approved.pack))
                { return Err(INTERNAL.into()); }
                state.outbox.push(entry.clone());
            } else if let Err(reason) = &evaluated {
                state.cards.insert(rec.id.to_string(), CardRetry::new(reason));
            }
            state.evaluated.insert(rec.id.to_string());
            Ok(true)
        });
        match reserved {
            Ok(false) => continue,
            Err(_) => {
                // If the outbox cannot be persisted there is no policy CAS.
                // Persist evaluation/card state separately when storage permits.
                let saved = update_state(home, |state| {
                    if state.evaluated.len() >= MAX_EVALUATED { return Err(INTERNAL.into()); }
                    state.evaluated.insert(rec.id.to_string());
                    state.cards.insert(rec.id.to_string(), CardRetry::new(INTERNAL));
                    Ok(())
                });
                if saved.is_ok() { refuse_with_retry(home, broker, &rec, INTERNAL).await; }
                else { refuse(home, broker, &rec, INTERNAL).await; }
                continue;
            }
            Ok(true) => {}
        }
        let entry = match evaluated {
            Ok((_, entry)) => entry,
            Err(reason) => { refuse_with_retry(home, broker, &rec, &reason).await; continue; }
        };
        if !duduclaw_core::gateway_instance::held(home) { return; }
        #[cfg(test)]
        tests::before_cas(home, broker, &rec).await;
        // Reuse the store's in-transaction digest guard and status CAS.
        let policy = entry.policy();
        let decision = crate::approval::EXPECTED_DECISION_DIGEST.scope(
            Some((rec.id.to_string(), crate::approval::decision_digest(&rec))),
            broker.decide_by_skill_trust_policy(&rec.id, &policy),
        ).await;
        outbox_budget -= 1;
        if let Err(error) = process_outbox(home, broker, entry).await {
            tracing::warn!(%error, "skill trust outbox remains pending");
        }
        if decision.is_err() {
            refuse_with_retry(home, broker, &rec, INTERNAL).await;
            continue;
        }
        match expert_skills::apply_skills_decision(home, &rec.agent_id).await {
            Ok(expert_skills::SkillsApplyOutcome::Enabled { not_enabled, .. }) if not_enabled.is_empty() => {},
            _ => tracing::warn!(approval_id = %rec.id, "skill trust approval applied incompletely; promotion digest gate retained"),
        }
    }
}

async fn refuse_with_retry(home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord, reason: &str) {
    append_audit_event(home, &AuditEvent::new("skills_auto_approval_refused", &rec.agent_id, Severity::Warning,
        serde_json::json!({"approval_id": rec.id.as_str(), "reason_code": reason_code(reason), "reason": reason})));
    retry_card(home, broker, rec).await;
}

#[cfg(test)]
pub(crate) mod tests;
