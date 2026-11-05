//! Bounded, metadata-only employee reply health. Never infer delivery from generation.
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const TURN_LOG: &str = "employee_health.jsonl";
const REASONS: &[&str] = &["empty_reply", "quota", "rate_limited", "auth_dead", "timeout", "gate_refused", "delivery", "other"];

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct TurnState {
    #[serde(default)]
    pub tracking_since: Option<String>,
    pub last_success_at: Option<String>,
    pub last_received_at: Option<String>,
    pub waiting_since: Option<String>,
    pub consecutive_failures: u32,
    pub last_reason: Option<String>,
    #[serde(default)]
    pub non_auth_failure: bool,
    #[serde(default)]
    pub alerted: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    enabled: bool,
    consecutive_failures: u32,
    no_success_hours: u32,
}
impl Default for Settings {
    fn default() -> Self { Self { enabled: true, consecutive_failures: 5, no_success_hours: 6 } }
}
impl Settings {
    fn load(home: &Path) -> Self {
        let disabled = Self { enabled: false, ..Self::default() };
        let raw = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(_) => return disabled,
        };
        let Ok(table) = raw.parse::<toml::Table>() else { return disabled; };
        let Some(section) = table.get("employee_health") else { return Self::default(); };
        let Ok(settings) = section.clone().try_into::<Self>() else { return disabled; };
        if !(1..=100).contains(&settings.consecutive_failures) || !(1..=168).contains(&settings.no_success_hours) { return disabled; }
        settings
    }
}
fn path(home: &Path, agent: &str) -> PathBuf { home.join("state/employee_health").join(format!("{agent}.json")) }
pub(crate) fn state(home: &Path, agent: &str) -> Result<TurnState, String> {
    if !duduclaw_core::is_valid_agent_id(agent) { return Err("invalid employee".into()); }
    match std::fs::read(path(home, agent)) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|_| "health state unreadable".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TurnState::default()),
        Err(_) => Err("health state unreadable".into()),
    }
}
fn mutate<T>(home: &Path, agent: &str, f: impl FnOnce(&mut TurnState) -> T) -> std::io::Result<T> {
    if !duduclaw_core::is_valid_agent_id(agent) { return Err(std::io::Error::other("invalid employee")); }
    let file = path(home, agent);
    std::fs::create_dir_all(file.parent().unwrap())?;
    duduclaw_core::with_file_lock(&file, || {
        let mut s = state(home, agent).map_err(std::io::Error::other)?;
        let result = f(&mut s);
        let tmp = file.with_extension("json.tmp");
        let raw = serde_json::to_vec(&s).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, raw)?;
        std::fs::rename(tmp, &file)?;
        Ok(result)
    })
}

pub(crate) async fn record_received(home: &Path, agent: &str) {
    let home = home.to_path_buf();
    let agent = agent.to_owned();
    if let Err(e) = tokio::task::spawn_blocking(move || record_received_sync(&home, &agent)).await {
        tracing::warn!(error = %e, "employee health receive worker failed");
    }
}
fn record_received_sync(home: &Path, agent: &str) {
    let now = Utc::now().to_rfc3339();
    let result = mutate(home, agent, |s| {
        let started = s.tracking_since.is_none();
        s.tracking_since.get_or_insert(now.clone());
        s.last_received_at = Some(now.clone());
        s.waiting_since.get_or_insert(now.clone());
        started
    });
    match result {
        Ok(true) => append_turn_event(home, agent, "employee_health_started", None),
        Err(e) => tracing::warn!(error = %e, "employee health receive record failed"),
        _ => {},
    }
}
fn append_turn_event(home: &Path, agent: &str, event: &str, reason: Option<&str>) {
    let mut row = json!({"event": event, "agent": agent, "timestamp": Utc::now().to_rfc3339(), "reason": reason});
    duduclaw_security::credential_detect::mask_json(&mut row);
    // Keep health telemetry out of the existing runtime failure audit stream.
    let path = home.join(TURN_LOG);
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{row}")
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "employee health audit append failed");
    }
}
pub(crate) async fn record_turn(home: &Path, agent: &str, reason: Option<&str>) {
    let home = home.to_path_buf();
    let agent = agent.to_owned();
    let reason = reason.map(str::to_owned);
    if let Err(e) = tokio::task::spawn_blocking(move || record_turn_sync(&home, &agent, reason.as_deref())).await {
        tracing::warn!(error = %e, "employee health turn worker failed");
    }
}
fn record_turn_sync(home: &Path, agent: &str, reason: Option<&str>) {
    if let Err(e) = mutate(home, agent, |s| {
        if let Some(reason) = reason {
            s.consecutive_failures = s.consecutive_failures.saturating_add(1);
            s.non_auth_failure |= reason != "auth_dead";
            s.last_reason = Some(if REASONS.contains(&reason) { reason } else { "other" }.into());
        } else {
            s.last_success_at = Some(Utc::now().to_rfc3339());
            s.waiting_since = None;
            s.consecutive_failures = 0;
            s.last_reason = None;
            s.non_auth_failure = false;
            s.alerted = false;
        }
    }) { tracing::warn!(error = %e, "employee health turn record failed"); }
    append_turn_event(home, agent, "employee_health_turn", reason);
}
fn due(s: &TurnState, cfg: Settings, now: DateTime<Utc>) -> bool {
    if !cfg.enabled || s.alerted { return false; }
    let stale = s.waiting_since.as_deref().and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .is_some_and(|t| now.signed_duration_since(t).num_hours() >= i64::from(cfg.no_success_hours));
    s.consecutive_failures >= cfg.consecutive_failures || stale
}

/// Fixed labels only. Provider text and conversation bodies never enter this surface.
fn reason_key(raw: &str) -> &'static str {
    match raw {
        "EmptyResponse" | "empty_response" | "empty_reply" => "empty_reply",
        "Billing" | "billing" | "AccountsCoolingDownLong" | "quota" => "quota",
        "RateLimited" | "rate_limited" | "AccountsCoolingDownShort" => "rate_limited",
        "AuthFailed" | "auth_failed" | "auth_dead" => "auth_dead",
        "Timeout" | "timeout" => "timeout",
        "gate_refused" | "spawn_gate" => "gate_refused",
        "delivery" => "delivery",
        _ => "other",
    }
}
#[derive(Default, Clone)]
pub(crate) struct FailureSnapshot {
    pub by_agent: HashMap<String, BTreeMap<String, u64>>,
    pub partial: bool,
}
fn aggregate(raw: &[u8], cut: bool, days: u32, now: DateTime<Utc>) -> FailureSnapshot {
    let mut out = FailureSnapshot { partial: cut, ..Default::default() };
    let text = String::from_utf8_lossy(raw);
    let text = if cut { text.split_once('\n').map(|(_, rest)| rest).unwrap_or("") } else { &text };
    let mut records = Vec::new();
    let mut tracking = HashMap::new();
    let mut has_new = std::collections::HashSet::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { out.partial = true; continue; };
        if let Some(agent) = v["agent"].as_str() {
            match v["event"].as_str() {
                Some("employee_health_started") => {
                    if let Some(ts) = v["timestamp"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()) {
                        tracking.insert(agent.to_string(), ts);
                    }
                }
                Some("employee_health_turn") => { has_new.insert(agent.to_string()); }
                _ => {},
            }
        }
        records.push(v);
    }
    for v in records {
        let Some(ts) = v["timestamp"].as_str().and_then(|v| DateTime::parse_from_rfc3339(v).ok()) else { out.partial = true; continue; };
        if ts > now || now.signed_duration_since(ts).num_seconds() > i64::from(days) * 86400 { continue; }
        let Some(agent) = v["agent"].as_str().or_else(|| v["agent_id"].as_str()) else { continue; };
        let Some(event) = v["event"].as_str() else { continue; };
        let key = match event {
            "employee_health_turn" => {
                let Some(reason) = v["reason"].as_str() else { continue; };
                reason_key(reason)
            }
            e if crate::channel_alerts::SEND_FAILURE_EVENTS.contains(&e) => "delivery",
            "channel_reply_fallback" => {
                // New turn events are the final post-gate outcome, not a second failure.
                if let Some(start) = tracking.get(agent) {
                    if ts >= *start { continue; }
                } else if has_new.contains(agent) {
                    out.partial = true;
                    continue;
                }
                if v["channel"].is_null() && !v["session_id"].as_str()
                    .is_some_and(|s| crate::trajectory_guard::channel_from_session_id(s).is_some()) { continue; }
                reason_key(v["reason"].as_str().unwrap_or("other"))
            }
            _ => continue,
        };
        *out.by_agent.entry(agent.into()).or_default().entry(key.into()).or_default() += 1;
    }
    out
}
fn read_tail(path: &Path) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        // Either stream can be absent on an install with no matching events.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    let start = meta.len().saturating_sub(TAIL_BYTES);
    let mut raw = Vec::new();
    file.seek(SeekFrom::Start(start))?;
    file.take(TAIL_BYTES).read_to_end(&mut raw)?;
    if start > 0 {
        // Trim each stream separately before merging; a cut record is not JSON.
        raw = raw.iter().position(|&b| b == b'\n')
            .map(|end| raw[end + 1..].to_vec()).unwrap_or_default();
    }
    Ok((raw, start > 0))
}
fn read_failures(home: &Path, days: u32) -> FailureSnapshot {
    let mut raw = Vec::new();
    let mut partial = false;
    for name in ["channel_failures.jsonl", TURN_LOG] {
        match read_tail(&home.join(name)) {
            Ok((tail, cut)) => {
                raw.extend_from_slice(&tail);
                if !raw.is_empty() && !raw.ends_with(b"\n") { raw.push(b'\n'); }
                partial |= cut;
            }
            Err(_) => partial = true,
        }
    }
    let mut snapshot = aggregate(&raw, false, days, Utc::now());
    snapshot.partial |= partial;
    snapshot
}
type Cache = HashMap<(PathBuf, u32), (std::time::Instant, FailureSnapshot)>;
pub(crate) fn failures(home: &Path, days: u32) -> FailureSnapshot {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = (home.to_path_buf(), days);
    if let Some((at, value)) = cache.get(&key) && at.elapsed().as_secs() < 30 { return value.clone(); }
    let value = read_failures(home, days);
    if cache.len() >= 8 { cache.clear(); }
    cache.insert(key, (std::time::Instant::now(), value.clone()));
    value
}
pub(crate) fn failure_counts(snapshot: &FailureSnapshot, agent: &str) -> Value {
    let mut counts = serde_json::Map::new();
    for key in REASONS {
        counts.insert((*key).into(), json!(snapshot.by_agent.get(agent).and_then(|m| m.get(*key)).copied().unwrap_or(0)));
    }
    Value::Object(counts)
}

/// Never sends to the triggering conversation: only verified, active owners/admins.
fn owner_targets(home: &Path, agent: &str) -> Result<Vec<(String, String)>, String> {
    let Some(db) = crate::decision_notify::open_user_db(home)? else { return Ok(Vec::new()); };
    let owners = db.get_agent_users(agent)?;
    let mut targets = Vec::new();
    for user in db.list_users()? {
        if user.status != duduclaw_auth::UserStatus::Active { continue; }
        if user.role != duduclaw_auth::UserRole::Admin && !owners.iter().any(|b|
            b.user_id == user.id && b.access_level == duduclaw_auth::AccessLevel::Owner) { continue; }
        for link in db.verified_channels_for_user(&user.id)? {
            let target = (link.channel, link.channel_user_id);
            if !targets.contains(&target) { targets.push(target); }
        }
    }
    Ok(targets)
}
fn claim_reminder(home: &Path, agent: &str, cfg: Settings) -> std::io::Result<bool> {
    mutate(home, agent, |s| {
        if !due(s, cfg, Utc::now()) { return false; }
        s.alerted = true;
        true
    })
}
async fn check_reminder(home: &Path, agent: &str) {
    let cfg = Settings::load(home);
    let Ok(s) = state(home, agent) else { return; };
    if !due(&s, cfg, Utc::now()) { return; }
    // The existing account-wide alarm already owns authentication outages.
    if !s.non_auth_failure && s.last_reason.as_deref() == Some("auth_dead") {
        if let Ok(raw) = std::fs::read(crate::auth_outage::state_path(home)) {
            if serde_json::from_slice::<crate::auth_outage::AuthOutageState>(&raw).is_ok_and(|s| s.active) { return; }
        }
    }
    let targets = match owner_targets(home, agent) {
        Ok(targets) if !targets.is_empty() => targets,
        _ => return,
    };
    // Persist the claim before I/O, once per episode across processes/restarts.
    let claim_home = home.to_path_buf();
    let claim_agent = agent.to_owned();
    let claimed = tokio::task::spawn_blocking(move || claim_reminder(&claim_home, &claim_agent, cfg)).await;
    if !matches!(claimed, Ok(Ok(true))) { return; }
    let text = "有 AI 員工持續無法完成回覆，請到儀表板的「員工健康」查看並處理。";
    let _ = crate::notify_push::push_health_notice(home, agent, text, targets).await;
}
pub(crate) async fn run(home: PathBuf, registry: std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
        if !duduclaw_core::gateway_instance::held(&home) { continue; }
        let agents: Vec<String> = registry.read().await.list().iter()
            .filter(|a| a.config.agent.status.is_listable(false)).map(|a| a.config.agent.name.clone()).collect();
        for agent in agents { check_reminder(&home, &agent).await; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn employee_health_aggregation_windows_and_categories() {
        let now = Utc::now();
        let records = [
            json!({"event":"channel_reply_fallback","agent":"a","channel":"line","reason":"AuthFailed","timestamp":now.to_rfc3339()}),
            json!({"event":"channel_reply_fallback","agent":"b","channel":"discord","reason":"Timeout","timestamp":(now-chrono::Duration::days(2)).to_rfc3339()}),
            json!({"event":"trajectory_anomaly","agent":"a","channel":"line","timestamp":now.to_rfc3339()}),
        ];
        let text = records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let daily = aggregate(text.as_bytes(), false, 1, now);
        assert_eq!(daily.by_agent["a"]["auth_dead"], 1);
        assert!(!daily.by_agent.contains_key("b"));
        assert_eq!(aggregate(text.as_bytes(), false, 7, now).by_agent["b"]["timeout"], 1);
        assert!(aggregate(b"broken\n", true, 1, now).partial);
    }
    #[test]
    fn employee_health_canonical_events_do_not_double_count_fallbacks() {
        let now = Utc::now();
        let records = [
            json!({"event":"employee_health_started", "agent":"a", "timestamp": (now-chrono::Duration::minutes(1)).to_rfc3339()}),
            json!({"event":"channel_reply_fallback", "agent":"a", "channel":"line", "reason":"Timeout", "timestamp":now.to_rfc3339()}),
            json!({"event":"employee_health_turn", "agent":"a", "reason":"timeout", "timestamp":now.to_rfc3339()}),
            json!({"event":"employee_health_turn", "agent":"a", "reason":"gate_refused", "timestamp":now.to_rfc3339()}),
            json!({"event":"employee_health_turn", "agent":"a", "reason":null, "timestamp":now.to_rfc3339()}),
        ];
        let text = records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let result = aggregate(text.as_bytes(), false, 1, now);
        assert_eq!(result.by_agent["a"]["timeout"], 1);
        assert_eq!(result.by_agent["a"]["gate_refused"], 1);
        assert_eq!(result.by_agent["a"].len(), 2);
    }
    #[tokio::test]
    async fn employee_health_turn_log_preserves_runtime_failure_audit() {
        let home = tempfile::tempdir().unwrap();
        record_received(home.path(), "a").await;
        let failure = json!({"event":"channel_reply_fallback", "agent":"a", "channel":"discord",
            "reason":"Timeout", "timestamp":Utc::now().to_rfc3339(),
            "runtime_failure":{"steps":[{"runtime":"codex", "reason":"Timeout"}]}});
        crate::trajectory_guard::append_anomaly(home.path(), &failure).unwrap();
        let before = std::fs::read(home.path().join("channel_failures.jsonl")).unwrap();
        record_turn(home.path(), "a", Some("timeout")).await;
        record_turn(home.path(), "a", Some("gate_refused")).await;
        assert_eq!(std::fs::read(home.path().join("channel_failures.jsonl")).unwrap(), before);
        let snapshot = read_failures(home.path(), 1);
        assert!(!snapshot.partial);
        assert_eq!(snapshot.by_agent["a"]["timeout"], 1);
        assert_eq!(snapshot.by_agent["a"]["gate_refused"], 1);
        assert_eq!(snapshot.by_agent["a"].len(), 2);
    }

    #[test]
    fn employee_health_tail_discards_cut_record_and_bounds_reads() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(TURN_LOG);
        let mut data = vec![b'x'; TAIL_BYTES as usize + 1];
        data.extend_from_slice(b"\n{}\n");
        std::fs::write(&path, data).unwrap();
        let (tail, cut) = read_tail(&path).unwrap();
        assert!(cut);
        assert_eq!(tail, b"{}\n");
        assert_eq!(read_tail(&home.path().join("absent.jsonl")).unwrap(), (Vec::new(), false));
    }

    #[tokio::test]
    async fn employee_health_reminder_dedup_and_recovery() {
        let home = tempfile::tempdir().unwrap();
        record_received(home.path(), "a").await;
        for _ in 0..5 { record_turn(home.path(), "a", Some("timeout")).await; }
        assert!(due(&state(home.path(), "a").unwrap(), Settings::default(), Utc::now()));
        assert!(claim_reminder(home.path(), "a", Settings::default()).unwrap());
        assert!(!claim_reminder(home.path(), "a", Settings::default()).unwrap());
        assert!(!due(&state(home.path(), "a").unwrap(), Settings::default(), Utc::now()));
        record_turn(home.path(), "a", None).await;
        assert_eq!(state(home.path(), "a").unwrap().consecutive_failures, 0);
        assert!(!state(home.path(), "a").unwrap().alerted);
    }
    #[test]
    fn employee_health_owner_targets_require_verified_active_owner_or_admin() {
        use duduclaw_auth::{AccessLevel, UserDb, UserRole};
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "admin", "isolated-test-password", UserRole::Admin).unwrap();
        let owner = db.create_user("owner@test.invalid", "owner", "isolated-test-password", UserRole::Employee).unwrap();
        let viewer = db.create_user("viewer@test.invalid", "viewer", "isolated-test-password", UserRole::Employee).unwrap();
        db.bind_agent(&owner.id, "a", AccessLevel::Owner).unwrap();
        db.bind_agent(&viewer.id, "a", AccessLevel::Viewer).unwrap();
        db.bind_channel_identity(&admin.id, "telegram", "admin-chat", true).unwrap();
        db.bind_channel_identity(&owner.id, "line", "owner-chat", true).unwrap();
        db.bind_channel_identity(&viewer.id, "discord", "viewer-chat", true).unwrap();
        db.bind_channel_identity(&owner.id, "telegram", "unverified-chat", false).unwrap();
        let targets = owner_targets(home.path(), "a").unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&("telegram".into(), "admin-chat".into())));
        assert!(targets.contains(&("line".into(), "owner-chat".into())));
        rusqlite::Connection::open(home.path().join("users.db")).unwrap()
            .execute("UPDATE users SET status='suspended' WHERE id=?1", [&owner.id]).unwrap();
        assert_eq!(owner_targets(home.path(), "a").unwrap().len(), 1);
    }
    #[test]
    fn employee_health_stale_requires_received_message_and_bad_config_disables() {
        let now = Utc::now();
        assert!(!due(&TurnState::default(), Settings::default(), now));
        let s = TurnState { waiting_since: Some((now-chrono::Duration::hours(6)).to_rfc3339()), ..Default::default() };
        assert!(due(&s, Settings::default(), now));
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[employee_health]\nno_success_hours=0\n").unwrap();
        assert!(!Settings::load(home.path()).enabled);
    }
}
