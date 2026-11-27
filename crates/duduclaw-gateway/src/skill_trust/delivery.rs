//! Durable post-CAS effects and independent manual-card retries.
use super::*;

const MAX_NOTICE_ATTEMPTS: u32 = 6;

// Zero is a valid captured position for an empty log, not a legacy marker.
fn missing_audit_since() -> u64 { u64::MAX }

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CardRetry {
    pub reason: String,
    pub card_attempts: u32,
    pub card_next_at: i64,
}
impl CardRetry {
    pub(super) fn new(reason: &str) -> Self {
        Self { reason: reason.to_owned(), card_attempts: 0, card_next_at: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NoticeState { Pending, Sent, Queued, NoTarget, GaveUp }

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Outbox {
    pub approval_id: String,
    pub slug: String,
    pub publisher: String,
    pub key_id: String,
    pub pack: String,
    pub version: String,
    pub manifest_sha256: String,
    pub skills: Vec<(String, String, String)>,
    pub attempts: u32,
    pub next_at: i64,
    #[serde(default = "missing_audit_since")]
    pub audit_since: u64,
    pub audit_done: bool,
    pub activity_done: bool,
    pub notice_state: NoticeState,
}
impl Outbox {
    pub(super) fn new(home: &Path, rec: &ApprovalRecord, approved: &AutoApproval) -> Result<Self, String> {
        let audit_since = duduclaw_security::audit::audit_log_position(home).map_err(|_| INTERNAL)?;
        Ok(Self {
            approval_id: rec.id.to_string(), slug: rec.agent_id.clone(),
            publisher: approved.publisher.clone(), key_id: approved.key_id.clone(),
            pack: approved.pack.clone(), version: approved.pack_version.clone(),
            manifest_sha256: approved.manifest_sha256.clone(), skills: approved.skills.clone(),
            attempts: 0, next_at: 0, audit_since, audit_done: false, activity_done: false,
            notice_state: NoticeState::Pending,
        })
    }
    fn approved(&self) -> AutoApproval {
        AutoApproval {
            publisher: self.publisher.clone(), key_id: self.key_id.clone(), pack: self.pack.clone(),
            pack_version: self.version.clone(), manifest_sha256: self.manifest_sha256.clone(), skills: self.skills.clone(),
        }
    }
    pub(super) fn policy(&self) -> String { format!("policy:skill_trust:{}:{}", self.publisher, self.key_id) }
    fn save(&self, home: &Path) -> Result<(), String> {
        update_state(home, |state| {
            let entry = state.outbox.iter_mut().find(|o| o.approval_id == self.approval_id).ok_or(INTERNAL)?;
            *entry = self.clone();
            Ok(())
        })
    }
}

fn backoff(attempts: u32) -> i64 {
    match attempts { 0 | 1 => 60, 2 => 120, 3 => 300, 4 => 600, 5 => 1800, _ => 3600 }
}

pub(super) async fn retry_card(home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord) {
    let Ok(Some(mut current)) = broker.get(&rec.id).await else { return; };
    if current.status != ApprovalStatus::Pending || current.is_stale(chrono::Utc::now())
        || current.notify_channel.is_some() || current.notify_chat_id.is_some()
    { return; }
    let now = chrono::Utc::now().timestamp();
    let attempt = update_state(home, |state| {
        let Some(card) = state.cards.get_mut(rec.id.as_str()) else { return Ok(None); };
        if card.card_next_at > now { return Ok(None); }
        card.card_attempts = card.card_attempts.saturating_add(1);
        card.card_next_at = now.saturating_add(backoff(card.card_attempts));
        Ok(Some(card.reason.clone()))
    });
    if let Ok(Some(reason)) = attempt {
        // Only the pushed copy contains the refusal explanation.
        current.summary.push_str(&format!("（未自動放行：{reason}）"));
        broker.push_new_request(&current).await;
    }
}

async fn notice(home: &Path, body: &str) -> NoticeState {
    #[cfg(test)]
    if let Some(state) = tests::capture_notice(home) { return state; }
    let targets = match crate::notify_push::admin_targets(home) {
        Ok(targets) => targets,
        Err(_) => return NoticeState::Pending,
    };
    if targets.is_empty() { return NoticeState::NoTarget; }
    let push = crate::notify_push::push_plain_notice(home, "skills_auto_approved", body, targets);
    match tokio::time::timeout(std::time::Duration::from_secs(15), push).await {
        Ok(receipt) => match receipt.outcome {
            crate::decision_notify::DeliverOutcome::Sent => NoticeState::Sent,
            crate::decision_notify::DeliverOutcome::Deferred => NoticeState::Queued,
            crate::decision_notify::DeliverOutcome::Failed => NoticeState::Pending,
        },
        Err(_) => NoticeState::Pending,
    }
}

pub(super) async fn process_outbox(home: &Path, broker: &ApprovalBroker, mut entry: Outbox) -> Result<Option<NoticeState>, String> {
    let id = crate::approval::ApprovalId::from(entry.approval_id.clone());
    let record = broker.get(&id).await?;
    let ours = record.as_ref().is_some_and(|rec| rec.status == ApprovalStatus::Approved
        && rec.decided_by.as_deref() == Some(entry.policy().as_str()) && rec.agent_id == entry.slug);
    if !ours {
        // A pending entry means a crash before CAS (or a failed CAS). Never
        // approve from an outbox alone: retain evaluation and return to humans.
        update_state(home, |state| {
            state.outbox.retain(|o| o.approval_id != entry.approval_id);
            if record.as_ref().is_some_and(|rec| rec.status == ApprovalStatus::Pending) {
                state.cards.insert(entry.approval_id.clone(), CardRetry::new(INTERNAL));
            }
            Ok(())
        })?;
        return Ok(None);
    }
    if entry.audit_since == missing_audit_since() {
        // Persist the legacy entry's first observed position before any append.
        // Replays must keep this position even if the acknowledgement is lost.
        entry.audit_since = duduclaw_security::audit::audit_log_position(home)?;
        entry.save(home)?;
    }
    if !entry.audit_done {
        let event = AuditEvent::new("skills_auto_approved", &entry.slug, Severity::Info,
            serde_json::json!({"approval_id": entry.approval_id, "publisher": entry.publisher,
                "key_id": entry.key_id, "pack": entry.pack, "version": entry.version,
                "manifest_sha256": entry.manifest_sha256,
                "skills": entry.skills.iter().map(|(agent, name, digest)| serde_json::json!({
                    "agent": agent, "skill": name, "digest": digest,
                })).collect::<Vec<_>>() }));
        // Only check records since the durable pre-CAS position, including an
        // append whose audit_done acknowledgement was lost in a crash.
        duduclaw_security::audit::try_append_audit_event_once(home, &event, entry.audit_since, &|value|
            value["event_type"] == "skills_auto_approved" && value["details"]["approval_id"] == entry.approval_id)?;
        entry.audit_done = true;
        entry.save(home)?;
    }
    let names = entry.skills.iter().map(|(agent, name, _)| format!("{agent} 的 {name}")).collect::<Vec<_>>().join("、");
    let body = format!("已依自家技能來源設定自動啟用 {} 個技能：{}；可在技能匯入頁停用", entry.skills.len(), names);
    if !entry.activity_done {
        let activity = crate::task_store::ActivityRow {
            id: format!("skill-trust:{}", entry.approval_id), event_type: "skills_auto_approved".into(),
            agent_id: entry.slug.clone(), task_id: None, summary: body.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(), metadata: Some(serde_json::json!({
                "approval_id": entry.approval_id, "publisher": entry.publisher, "key_id": entry.key_id,
            }).to_string()),
        };
        let store = crate::task_store::TaskStore::open(home).map_err(|e| e.to_string())?;
        store.append_activity_once(&activity).await.map_err(|e| e.to_string())?;
        entry.activity_done = true;
        entry.save(home)?;
    }
    if entry.notice_state == NoticeState::Pending {
        let now = chrono::Utc::now().timestamp();
        if entry.next_at > now { return Ok(None); }
        if entry.attempts >= MAX_NOTICE_ATTEMPTS {
            entry.notice_state = NoticeState::GaveUp;
        } else {
            // Persist the attempt/backoff before network IO, including crashes.
            entry.attempts += 1;
            entry.next_at = now.saturating_add(backoff(entry.attempts));
            entry.save(home)?;
            entry.notice_state = notice(home, &body).await;
            if entry.notice_state == NoticeState::Pending && entry.attempts >= MAX_NOTICE_ATTEMPTS {
                entry.notice_state = NoticeState::GaveUp;
            }
        }
        entry.save(home)?;
    }
    if entry.notice_state == NoticeState::Pending { return Ok(None); }
    if entry.notice_state == NoticeState::GaveUp {
        tracing::warn!(approval_id = %entry.approval_id, "skill trust notice gave_up; activity retained");
    }
    // Atomic finalization: only a successful policy CAS reaches this point.
    // An older outbox finishing late must never lower an existing high-water mark.
    update_state(home, |state| {
        let approved = entry.approved();
        match state.highest_versions.iter_mut().find(|v| v.publisher == entry.publisher && v.pack == entry.pack) {
            Some(old) => match Version::parse(&entry.version)?.compare(&Version::parse(&old.version)?) {
                Ordering::Greater => { old.version = entry.version.clone(); old.manifest_sha256 = entry.manifest_sha256.clone(); }
                Ordering::Equal if old.manifest_sha256 != entry.manifest_sha256 => return Err(VERSION.into()),
                _ => {}
            },
            None => {
                if state.highest_versions.len() >= 4096 { return Err(INTERNAL.into()); }
                state.highest_versions.push(HighestVersion::from(&approved));
            }
        }
        state.outbox.retain(|o| o.approval_id != entry.approval_id);
        Ok(())
    })?;
    Ok(Some(entry.notice_state))
}
