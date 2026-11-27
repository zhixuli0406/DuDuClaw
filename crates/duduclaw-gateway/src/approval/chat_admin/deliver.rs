//! RM-143 delivery authority comes exclusively from live Admins' own proofs.
use std::path::Path;

use chrono::Utc;
use duduclaw_auth::{otp::ChatApprovalProof, UserDb, UserRole};
use serde_json::json;

use crate::approval::{ApprovalBroker, ApprovalRecord, ApprovalStatus, ApprovalStore};
use super::{card, config, receipts, chat_tier, Tier};

pub(super) mod transport;
#[cfg(test)]
pub(crate) mod tests;

#[derive(Default, Debug)]
pub(crate) struct DeliveryResult {
    pub(crate) delivered: usize,
    pub(crate) first: Option<(String, String)>,
}

impl DeliveryResult {
    fn record(&mut self, target: &Target) {
        self.delivered += 1;
        if self.first.is_none() {
            self.first = Some((target.proof.channel.clone(), target.proof.proof_dm.clone()));
        }
    }
}

#[derive(Clone)]
pub(super) struct Target {
    pub(super) user_id: String,
    pub(super) proof: ChatApprovalProof,
    pub(super) agent: Option<String>,
}

struct Plan {
    recipients: Vec<Vec<Target>>,
    card: card::ChatCard,
    digest: String,
    tier: &'static str,
    valid_until: String,
}

fn eligible(home: &Path, db: &UserDb, rec: &ApprovalRecord) -> Option<config::ChatAdminConfig> {
    let cfg = config::effective(home, Some(db))?;
    if !matches!(chat_tier(Some(home), rec), Tier::T1 | Tier::T2) { return None; }
    if let Some(employee) = crate::personal_decision::card_employee(home, rec).ok()? {
        if crate::personal_visibility::personal_owner(home, &employee).ok()?.is_some() { return None; }
    }
    Some(cfg)
}

/// `push` has no registry handle. Use the same fail-closed enumeration as P5.
fn agents(home: &Path) -> Option<Vec<String>> {
    match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => {
            let mut ids = Vec::new();
            for entry in entries {
                let entry = entry.ok()?;
                if entry.file_type().ok()?.is_dir() {
                    ids.push(entry.file_name().into_string().ok()?);
                }
            }
            Some(ids)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    }
}

fn private_proof(proof: &ChatApprovalProof) -> bool {
    crate::knowledge::add_source_chat::private_destination(&proof.channel, &proof.proof_dm)
        && (proof.channel != "telegram" || proof.proof_dm == proof.channel_user_id)
}

fn recipients(home: &Path, db: &UserDb, cfg: &config::ChatAdminConfig, rec: &ApprovalRecord) -> Option<Vec<Vec<Target>>> {
    let agents = agents(home)?;
    let mut recipients = Vec::new();
    for user in db.list_users().ok()? {
        if user.id == "system" || uuid::Uuid::parse_str(&user.id).ok()
            .is_none_or(|id| id.to_string() != user.id) { continue; }
        let Ok(Some(live)) = db.live_user_context(&user.id) else { continue; };
        if live.role != UserRole::Admin || live.must_change_password { continue; }
        let me = duduclaw_core::HumanPrincipal::user(&user.id).map(|p| p.to_string());
        if crate::on_behalf_of::self_approval::refuses(home, rec.on_behalf_of.as_deref(), me.as_deref(), true) { continue; }
        let Ok(proofs) = db.approval_proofs(&user.id) else { continue; };
        let mut choices = Vec::new();
        let mut unavailable = false;
        for channel in &cfg.channels {
            if !proofs.iter().any(|proof| proof.channel == channel.as_str() && private_proof(proof)) { continue; }
            let accounts = match crate::chat_link_intercept::configured_accounts(home, channel.as_str(), &agents) {
                Ok(accounts) => accounts,
                // An absent/PubSub app is not a read error: try the next channel.
                Err(crate::chat_link_intercept::NO_CONFIGURED_ACCOUNTS) => continue,
                Err(_) => { unavailable = true; break; }
            };
            if let Some((proof, account)) = proofs.iter()
                .filter(|proof| proof.channel == channel.as_str() && private_proof(proof))
                .find_map(|proof| accounts.iter().find(|account| account.account == proof.proof_account)
                    .map(|account| (proof, account))) {
                choices.push(Target { user_id: user.id.clone(), proof: proof.clone(), agent: account.agent_id.clone() });
            }
        }
        if !unavailable && !choices.is_empty() { recipients.push(choices); }
    }
    Some(recipients)
}

fn prepare(home: &Path, rec: &ApprovalRecord) -> Option<Plan> {
    let db = crate::decision_notify::open_user_db(home).ok()??;
    let cfg = eligible(home, &db, rec)?;
    if rec.status != ApprovalStatus::Pending { return None; }
    let now = Utc::now();
    let valid_until = config::card_valid_until(&cfg, now, rec.expires_at()?)?;
    if valid_until <= now { return None; }
    // Freeze the displayed record's digest before any network operation.
    let digest = crate::approval::live_events::decision_digest_at(home, rec);
    let card = card::build(home, rec, valid_until, crate::trigger_route::texts::lang_for(home))?;
    Some(Plan { recipients: recipients(home, &db, &cfg, rec)?, card, digest,
        tier: match chat_tier(Some(home), rec) { Tier::T1 => "T1", Tier::T2 => "T2", Tier::T3 => return None },
        valid_until: valid_until.to_rfc3339() })
}

/// None means the existing route owns this record (including mail-requester).
/// Some(0) means RM-143 could not deliver and must use the plain notice.
pub(crate) async fn try_push(home: &Path, rec: &ApprovalRecord, reminder: bool) -> Option<DeliveryResult> {
    let (owned_home, owned_rec) = (home.to_path_buf(), rec.clone());
    let broker = tokio::task::spawn_blocking(move || {
        let db = crate::decision_notify::open_user_db(&owned_home).ok()??;
        eligible(&owned_home, &db, &owned_rec)?;
        Some(ApprovalBroker::open(&owned_home))
    }).await.ok().flatten()?;
    let Ok(broker) = broker else { return Some(DeliveryResult::default()); };
    Some(if reminder { remind(home, &broker.store, rec).await } else { deliver(home, &broker, rec).await })
}

pub(crate) async fn deliver(home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord) -> DeliveryResult {
    let (owned_home, owned_rec) = (home.to_path_buf(), rec.clone());
    let Some(plan) = tokio::task::spawn_blocking(move || prepare(&owned_home, &owned_rec)).await.ok().flatten() else {
        return DeliveryResult::default();
    };
    run(home, &broker.store, rec, plan, &transport::Network::new(home)).await
}

/// Reuse the delivery plan, but retain only the receipt owner's destinations.
/// Live role, proof, configuration and content are re-read before sending.
pub(crate) async fn deliver_to_user(
    home: &Path, broker: &ApprovalBroker, rec: &ApprovalRecord, user_id: &str,
) -> DeliveryResult {
    let (owned_home, owned_rec, user) = (home.to_path_buf(), rec.clone(), user_id.to_owned());
    let Some(plan) = tokio::task::spawn_blocking(move || {
        prepare_for_user(&owned_home, &owned_rec, &user)
    }).await.ok().flatten() else { return DeliveryResult::default(); };
    run(home, &broker.store, rec, plan, &transport::Network::new(home)).await
}

fn prepare_for_user(home: &Path, rec: &ApprovalRecord, user_id: &str) -> Option<Plan> {
    let mut plan = prepare(home, rec)?;
    plan.recipients.retain(|choices| choices.first().is_some_and(|t| t.user_id == user_id));
    Some(plan)
}

#[async_trait::async_trait]
pub(super) trait Sender: Sync {
    async fn supports(&self, target: &Target) -> bool;
    async fn send(&self, target: &Target, card: &card::ChatCard, approval_id: &str) -> Result<Option<crate::decision_card::PushedMessage>, ()>;
    async fn replace(&self, target: &Target, message: Option<&crate::decision_card::PushedMessage>) -> bool;
    async fn remind(&self, target: &Target, message: Option<&str>, text: &str) -> bool;
    async fn edit(&self, target: &Target, message: &crate::decision_card::PushedMessage, text: &str) -> bool;
}

async fn run(home: &Path, store: &ApprovalStore, rec: &ApprovalRecord, plan: Plan, sender: &impl Sender) -> DeliveryResult {
    let mut result = DeliveryResult::default();
    for choices in plan.recipients {
        for target in choices {
            if !sender.supports(&target).await { continue; }
            // Try supported/proven channels in order until one card is delivered.
            let sent = sender.send(&target, &plan.card, rec.id.as_str()).await;
            if let Ok(message) = sent {
                if message.is_none() && target.proof.channel != "line" {
                    if !sender.replace(&target, message.as_ref()).await {
                        tracing::warn!(channel = %target.proof.channel, "chat-admin card replacement failed; no receipt grants authority");
                    }
                    continue;
                }
                let receipt = receipts::Receipt {
                    card_id: uuid::Uuid::new_v4().to_string(), approval_id: rec.id.as_str().into(),
                    purpose: "rm143".into(), user_id: target.user_id.clone(), channel: target.proof.channel.clone(),
                    account: target.proof.proof_account.clone(), principal: target.proof.channel_user_id.clone(),
                    dm: target.proof.proof_dm.clone(),
                    message_id: if target.proof.channel == "line" { None } else { message.as_ref().map(|m| m.message_id.clone()) },
                    digest: plan.digest.clone(), tier: plan.tier.into(), valid_until: plan.valid_until.clone(),
                    status: "live".into(), sent_at: Utc::now().to_rfc3339(),
                };
                if receipts::insert(store, &receipt).await.is_err() {
                    if !sender.replace(&target, message.as_ref()).await {
                        tracing::warn!(channel = %target.proof.channel, "chat-admin card replacement failed; no receipt grants authority");
                    }
                } else {
                    result.record(&target);
                    audit(home, rec, &target).await;
                }
                break;
            }
        }
    }
    result
}

async fn audit(home: &Path, rec: &ApprovalRecord, target: &Target) {
    use duduclaw_security::audit::{AuditEvent, Severity};
    let event = AuditEvent::new("approval_chat_admin_card_sent", &rec.agent_id, Severity::Info,
        json!({"approval_id":rec.id.as_str(), "kind":rec.action_kind, "user_id":target.user_id,
            "channel":target.proof.channel, "account":target.proof.proof_account}));
    let home = home.to_path_buf();
    if !matches!(tokio::task::spawn_blocking(move || duduclaw_security::audit::try_append_audit_event(&home, &event)).await, Ok(Ok(()))) {
        tracing::warn!("chat-admin card delivery audit unavailable");
    }
}

async fn remind(home: &Path, store: &ApprovalStore, rec: &ApprovalRecord) -> DeliveryResult {
    let Ok(live) = receipts::list_live(store, &rec.id).await else { return DeliveryResult::default(); };
    let (owned_home, owned_rec) = (home.to_path_buf(), rec.clone());
    let Some(plan) = tokio::task::spawn_blocking(move || prepare(&owned_home, &owned_rec)).await.ok().flatten() else {
        return DeliveryResult::default();
    };
    let lang_home = home.to_path_buf();
    let Ok(lang) = tokio::task::spawn_blocking(move || crate::trigger_route::texts::lang_for(&lang_home)).await else {
        return DeliveryResult::default();
    };
    remind_live(&live, &plan, &transport::Network::new(home), card::reminder_text(lang)).await
}

async fn remind_live(live: &[receipts::Receipt], plan: &Plan, sender: &impl Sender, text: &str) -> DeliveryResult {
    let mut result = DeliveryResult::default();
    for receipt in live.iter().filter(|r| r.purpose == "rm143" && r.status == "live") {
        if !chrono::DateTime::parse_from_rfc3339(&receipt.valid_until).is_ok_and(|t| t > Utc::now()) { continue; }
        let target = plan.recipients.iter().flatten().find(|t| t.user_id == receipt.user_id
            && t.proof.channel == receipt.channel && t.proof.proof_account == receipt.account
            && t.proof.channel_user_id == receipt.principal && t.proof.proof_dm == receipt.dm);
        let Some(target) = target else { continue; };
        if sender.remind(target, receipt.message_id.as_deref(), text).await { result.record(target); }
    }
    result
}
