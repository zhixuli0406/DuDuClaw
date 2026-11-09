//! Google tenant credential sources for protected mode (native Linux
//! isolation v1.81, task E4).
//!
//! A tenant source is existing, admin-configured product configuration; this
//! module adds no secret store:
//!
//! - **Delegate** (domain-wide delegation): the sealed delegation key
//!   `<home>/protected/google-service-account.sealed` (imported by an Admin
//!   through `google.delegation.import_key`) plus the employee's admin-written
//!   `agent.toml [google] identity = "delegate:<subject>"`.
//! - **ServiceAccount**: `config.toml [integrations.google_service_account]`
//!   (`key_file` + `subject`), used when the employee's identity is `home`.
//!
//! Both are served only through the employee's own SharedTenant Google route
//! in the C1 catalog (provisioned by C3 with admin step-up), so only Shared
//! employees of the route's tenant can reach them.
//!
//! **Subject rule (Decision D-5).** The subject a tenant-source token is
//! minted for comes only from the admin configuration above, never from
//! employee arguments, the run environment or run metadata. It is admitted
//! only when, comparing normalized emails by exact equality against users.db:
//! - it is listed in `[integrations.google_identity] service_mailboxes` and
//!   matches NO users.db account (a person's mailbox is never a service
//!   mailbox): a Shared action, for any requester the Shared employee's
//!   authority admitted; or
//! - (Delegate only) it matches exactly ONE users.db account, that account is
//!   Active, an Admin ATTESTED that the subject belongs to exactly that
//!   account (SR4 item 1: `google.delegation.attest_subject`, users.db
//!   `google_delegate_attestations`, revoked by any later email change of the
//!   account), and the authenticated requester is that account (exact user
//!   id): a Personal-like action, on behalf of the requesting human only. An
//!   email match alone never admits (b): staff can set their own login email
//!   to any unused address. No attestation ⇒ refused (existing delegate
//!   configurations stay refused until an Admin attests; the dashboard shows
//!   `attestation_required`, [`subject_notice`]).
//!
//! Anything else (an undesignated non-person subject, a person mailbox that
//! is not the requester, an ambiguous email, a System requester for a person
//! mailbox, a service account acting as a person) is refused. Tools are
//! further limited to `[integrations.google_identity] tenant_scopes` (each
//! entry must be one of the minted scopes; empty serves no data tool), and
//! the minted assertion asks Google for exactly those scopes (SR4 item 5).
//!
//! SR4 item 2: a stored users.db (or history) email the normalizer rejects is
//! `Unavailable`, never skipped — unless it provably names no mailbox at all
//! (no `@`, or a domain without a dot such as the bootstrap `admin@local`).
//! A staff member who sets their own email to a designated mailbox or a
//! delegate subject still refuses it (fail closed), but the conflict is shown
//! to Admins ([`subject_notice`]) and audited by the bridge.
//!
//! SR4 round 3: only a pure-ASCII value can be inert (N3); an attestation
//! binds the live employee id and incarnation of the label, not the label
//! alone (N2, [`Incarnation`]); the Admin notice names the holder an
//! attestation would bind (N1); and an Admin identity test is admitted only
//! for a subject the rule could admit ([`admit_for_identity_test`]).
//!
//! Every read fails closed: unreadable or malformed config or key material is
//! `Unavailable`; a configuration that would let a failure fall back to a
//! home source (`fallback = "home"`, an Apps Script bridge beside the
//! service account) is `Refused`.
use std::{io::Read as _, path::Path};

use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{self as gi, Effective, Fallback};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::VerificationError;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Largest sealed delegation record read here (the importer caps the key at 16 KiB).
const MAX_SEALED_RECORD: u64 = 64 * 1024;
/// Largest service-account key file read here (same bound as the SA owner).
const MAX_KEY_FILE: u64 = 16 * 1024;
/// Largest `config.toml` read here (same bound as the W3 source observer).
const MAX_CONFIG: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantSourceKind {
    /// Domain-wide delegation through the sealed delegation key.
    Delegate,
    /// `[integrations.google_service_account]`; never acts as a person.
    ServiceAccount,
}
impl TenantSourceKind {
    fn as_str(self) -> &'static str {
        match self { Self::Delegate => "delegate", Self::ServiceAccount => "service_account" }
    }
}

/// Non-sensitive refusals. Every variant refuses the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TenantSourceError {
    /// Config or key material cannot be read or is malformed.
    Unavailable,
    /// The configuration is readable but not admissible in protected mode.
    Refused,
}
impl From<TenantSourceError> for VerificationError {
    fn from(error: TenantSourceError) -> Self {
        match error {
            TenantSourceError::Unavailable => Self::Unavailable,
            TenantSourceError::Refused => Self::BindingMismatch,
        }
    }
}

/// One configured tenant source as of one read. Holds no secret: key
/// material is represented only by a domain-separated digest of its bytes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TenantSource {
    kind: TenantSourceKind,
    subject: String,
    service_mailboxes: Vec<String>,
    scopes: Vec<String>,
    material: String,
}
impl std::fmt::Debug for TenantSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantSource").field("kind", &self.kind).finish_non_exhaustive()
    }
}
impl TenantSource {
    pub(crate) fn kind(&self) -> TenantSourceKind { self.kind }
    pub(crate) fn subject(&self) -> &str { &self.subject }
    /// SR4 item 5: the configured `tenant_scopes` (each a minted scope).
    pub(crate) fn scopes(&self) -> &[String] { &self.scopes }
    /// `google_status` is metadata only; every data tool needs a configured scope.
    pub(crate) fn serves(&self, tool: &str) -> bool {
        tool == "google_status" || crate::google_personal::scopes::permitted(tool, &self.scopes)
    }
    /// The source proof the bridge pins per tool. Changes to the subject,
    /// mailbox designation, scopes or key material all change it.
    pub(crate) fn digest(&self) -> String {
        gi::google_arguments_digest(&json!({"tenant_source": self.kind.as_str(), "subject": self.subject,
            "service_mailboxes": self.service_mailboxes, "scopes": self.scopes, "material": self.material}))
    }
    #[cfg(test)]
    pub(crate) fn fixture(kind: TenantSourceKind, subject: &str, service_mailboxes: &[&str], scopes: &[&str]) -> Self {
        Self { kind, subject: subject.into(), service_mailboxes: service_mailboxes.iter().map(|s| (*s).into()).collect(),
            scopes: scopes.iter().map(|s| (*s).into()).collect(), material: "fixture".into() }
    }
}

/// Who asks, as established by the bridge from the capability principal and
/// the host's live run association only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Actor<'a> {
    /// An authenticated person (users.db id).
    Human(&'a str),
    /// Background / scheduled / delegated work; never a person.
    System,
}

/// A users.db account whose normalized email equals the subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubjectAccount {
    pub(crate) user_id: String,
    pub(crate) active: bool,
    /// SR4 item 1: an Admin attested that this employee's delegate subject
    /// belongs to exactly this account, and its email has not changed since.
    pub(crate) attested: bool,
}

/// Which branch of the subject rule admitted the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubjectUse {
    ServiceMailbox,
    RequestingUser,
}

/// SR4 item 2(iii): a stored login (or history) email, normalized like the
/// subject. `Ok(None)`: it provably names no mailbox (no `@`, or a domain
/// without a dot — e.g. the bootstrap `admin@local`), so it can never be the
/// subject under any normalizer. Anything else the normalizer rejects is
/// `Unavailable`: an address we cannot compare is never assumed to differ.
///
/// SR4 round 3 (N3): only a pure-ASCII value can be inert. A non-ASCII
/// address the normalizer rejects (a full-width `＠`, an ideographic full stop
/// `。`, an IDN domain) may well name a mailbox under another normalizer, so
/// it is `Unavailable`.
pub(crate) fn stored_email(raw: &str) -> Result<Option<String>, VerificationError> {
    if let Ok(email) = gi::normalize_email(raw) { return Ok(Some(email)); }
    if !raw.is_ascii() { return Err(VerificationError::Unavailable); }
    let lowered = raw.trim().to_ascii_lowercase();
    let inert = match lowered.rsplit_once('@') {
        None => true,
        Some((_, domain)) => {
            let domain = domain.trim_end_matches('.');
            !domain.is_empty() && !domain.contains('.')
        }
    };
    if inert { Ok(None) } else { Err(VerificationError::Unavailable) }
}

/// Every users.db account whose email, normalized with the same normalizer as
/// the configured subject, is exactly `subject`. Unreadable, or a stored email
/// that cannot be compared ([`stored_email`]) ⇒ `Unavailable`.
///
/// SR5 item 4: when no current account holds `subject` but one EVER did
/// (`account_email_history`), a former-holder entry (no user id, inactive) is
/// returned, so a designated service mailbox that was a person's login
/// address at any time is refused at use time too ([`admit_subject`]).
///
/// SR4 item 1: a single current holder carries whether `agent`'s Admin
/// attestation binds exactly `subject` to that account (unreadable ⇒
/// `Unavailable`). SR4 round 3 (N2): and to exactly `employee`, the live
/// employee id and incarnation holding the label; `None` (not known) never
/// counts as attested.
pub(crate) fn subject_accounts(db: &UserDb, agent: &str, employee: Option<Incarnation>, subject: &str)
    -> Result<Vec<SubjectAccount>, VerificationError> {
    let users = db.list_users().map_err(|_| VerificationError::Unavailable)?;
    let mut accounts = Vec::new();
    for user in users {
        if stored_email(&user.email)?.as_deref() == Some(subject) {
            accounts.push(SubjectAccount { active: user.status == duduclaw_auth::models::UserStatus::Active,
                user_id: user.id, attested: false });
        }
    }
    if accounts.is_empty() {
        let history = db.account_email_history().map_err(|_| VerificationError::Unavailable)?;
        let mut held = false;
        for email in &history {
            held |= stored_email(email)?.as_deref() == Some(subject);
        }
        if held { accounts.push(SubjectAccount { user_id: String::new(), active: false, attested: false }); }
    }
    if let [account] = accounts.as_mut_slice()
        && !account.user_id.is_empty()
    {
        let state = db.google_delegate_attestation(agent).map_err(|_| VerificationError::Unavailable)?;
        account.attested = match (state, employee) {
            (Some(state), Some((id, incarnation))) =>
                state.binds(subject, &account.user_id, &hex::encode(id.0), incarnation),
            _ => false,
        };
    }
    Ok(accounts)
}

/// SR4 round 3 (N2): an employee's immutable registry id and incarnation.
/// An attestation binds both; a recreated label (new id) or a restore (next
/// incarnation) needs a fresh attestation.
pub(crate) type Incarnation = (EmployeeId, u32);

/// The live employee id and incarnation holding `agent`, `None` when the
/// lifecycle cannot be read (then nothing is attested). Blocking (SQLite +
/// root IPC).
pub(crate) fn current_incarnation(home: &Path, agent: &str) -> Option<Incarnation> {
    crate::isolation_lifecycle::read_lifecycle(home, agent).ok()
        .map(|lifecycle| (lifecycle.employee, lifecycle.incarnation))
}

/// Where a configured subject stands under D-5, independent of the requester
/// (the requester check is [`admit_subject`]'s). Also the Admin notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubjectState {
    /// (a) designated, and no account holds or ever held it.
    ServiceMailbox,
    /// Designated, but a users.db account holds or held the address (a person's
    /// mailbox, or a staff member who set their own email to it). Refused.
    MailboxHeldByAccount,
    /// (b) one Active account, attested by an Admin: that person only.
    AttestedPerson,
    /// One Active account, but no binding Admin attestation. Refused.
    AttestationRequired,
    /// More than one account has the address. Refused.
    Ambiguous,
    /// The one holder is not Active, or only held it formerly. Refused.
    PersonInactive,
    /// Neither a designated mailbox nor a delegate person (incl. a service
    /// account naming a person). Refused.
    NotAdmissible,
}
impl SubjectState {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::ServiceMailbox => "service_mailbox",
            Self::MailboxHeldByAccount => "service_mailbox_conflict",
            Self::AttestedPerson => "attested_person",
            Self::AttestationRequired => "attestation_required",
            Self::Ambiguous => "ambiguous",
            Self::PersonInactive => "person_inactive",
            Self::NotAdmissible => "not_admissible",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::ServiceMailbox => "服務信箱：經授權的請求者都可透過這位員工使用。",
            Self::MailboxHeldByAccount => "這個服務信箱是（或曾是）某位同仁的登入帳號，已停止使用。若是同仁自行改成此信箱，請他改回；否則請改用其他服務信箱。",
            Self::AttestedPerson => "已由管理員確認此信箱屬於該同仁；只在該同仁本人提出請求時代表本人使用。",
            Self::AttestationRequired => "需要管理員確認這個信箱屬於哪位同仁，確認前不會代表任何人使用。",
            Self::Ambiguous => "有多個帳號使用這個信箱，無法判斷屬於誰，已停止使用。",
            Self::PersonInactive => "這個信箱的同仁目前不是有效帳號，已停止使用。",
            Self::NotAdmissible => "這個信箱既不是指定的服務信箱，也不是經管理員確認的同仁信箱，已停止使用。",
        }
    }
}

/// The subject's standing under D-5. Pure: callers supply the users.db facts.
pub(crate) fn subject_state(source: &TenantSource, accounts: &[SubjectAccount]) -> SubjectState {
    let designated = source.service_mailboxes.iter().any(|mailbox| mailbox == &source.subject);
    match (designated, accounts) {
        (true, []) => SubjectState::ServiceMailbox,
        // A person's account can never be designated as a service mailbox.
        (true, _) => SubjectState::MailboxHeldByAccount,
        // A service account never acts as a person.
        (false, _) if source.kind != TenantSourceKind::Delegate => SubjectState::NotAdmissible,
        (false, []) => SubjectState::NotAdmissible,
        (false, [account]) if account.user_id.is_empty() || !account.active => SubjectState::PersonInactive,
        (false, [account]) if account.attested => SubjectState::AttestedPerson,
        (false, [_]) => SubjectState::AttestationRequired,
        (false, _) => SubjectState::Ambiguous,
    }
}

/// The subject rule (D-5). Pure: callers supply the users.db facts.
pub(crate) fn admit_subject(source: &TenantSource, actor: Actor<'_>, accounts: &[SubjectAccount])
    -> Result<SubjectUse, VerificationError> {
    match (subject_state(source, accounts), actor, accounts) {
        (SubjectState::ServiceMailbox, _, _) => Ok(SubjectUse::ServiceMailbox),
        (SubjectState::AttestedPerson, Actor::Human(user_id), [account]) if account.user_id == user_id =>
            Ok(SubjectUse::RequestingUser),
        _ => Err(VerificationError::BindingMismatch),
    }
}

/// SR4 round 3 (item 5 residual): an Admin's `google.identity.test` mints
/// tokens for the configured subject without any requester, so in protected
/// mode it is admitted only where the D-5 rule could admit SOME requester:
/// a designated service mailbox, or a person an Admin attested for exactly
/// this employee incarnation ([`admit_subject`]'s states). Anything else —
/// notably an unattested delegate person — is refused with the Admin notice
/// text, before anything is minted. Blocking reads.
pub(crate) fn admit_for_identity_test(home: &Path, agent: &str, db: Option<&UserDb>, employee: Option<Incarnation>)
    -> Result<(), String> {
    let source = match read(home, agent) {
        Ok(Some(source)) => source,
        Ok(None) => return Err(SubjectState::NotAdmissible.message().to_owned()),
        Err(TenantSourceError::Unavailable) => return Err(SOURCE_UNAVAILABLE.to_owned()),
        Err(TenantSourceError::Refused) => return Err(SOURCE_REFUSED.to_owned()),
    };
    let db = db.ok_or_else(|| USERS_UNAVAILABLE.to_owned())?;
    let accounts = subject_accounts(db, agent, employee, source.subject()).map_err(|_| USERS_UNAVAILABLE.to_owned())?;
    match subject_state(&source, &accounts) {
        SubjectState::ServiceMailbox | SubjectState::AttestedPerson => Ok(()),
        refused => Err(refused.message().to_owned()),
    }
}

const SOURCE_UNAVAILABLE: &str = "Google 租戶來源設定或金鑰無法讀取，已停止使用。";
const SOURCE_REFUSED: &str = "這個 Google 設定在受保護模式下不允許（例如失敗時改用全公司帳號）。";
const USERS_UNAVAILABLE: &str = "帳號資料無法讀取，或有無法辨識的登入 email，無法確認信箱歸屬，已停止使用。";

/// SR4 items 1 / 2(iv): the Admin notice of `agent`'s tenant-source subject
/// (`null` when its Google identity is no tenant source). Blocking reads.
///
/// SR4 round 3 (N1 (2)): for `attestation_required` and `attested_person`
/// the notice names the single current holder the Admin attests (or
/// attested): `holder {user_id, name, created_at, email_self_set}` —
/// `email_self_set` when the account itself set the current address. (N2) An
/// attestation counts only for the live employee incarnation of the label.
///
/// SR4 round 3 follow-up: the `holder` block (another account's name and
/// creation time) is attached only when `include_holder` — the Admin path
/// (`google.delegation.get`). `agents.google_identity_get` (Viewer) never
/// receives it.
pub(crate) fn subject_notice(home: &Path, agent: &str, db: Option<&UserDb>, include_holder: bool) -> serde_json::Value {
    subject_notice_with(home, agent, db, include_holder, || current_incarnation(home, agent))
}

/// [`subject_notice`] with the live employee incarnation supplied (read only
/// when the employee has a tenant source).
pub(crate) fn subject_notice_with(home: &Path, agent: &str, db: Option<&UserDb>, include_holder: bool,
    employee: impl FnOnce() -> Option<Incarnation>) -> serde_json::Value {
    let notice = |state: &str, message: &str| json!({ "state": state, "message": message });
    let source = match read(home, agent) {
        Ok(None) => return serde_json::Value::Null,
        Ok(Some(source)) => source,
        Err(TenantSourceError::Unavailable) => return notice("unavailable", SOURCE_UNAVAILABLE),
        Err(TenantSourceError::Refused) => return notice("refused", SOURCE_REFUSED),
    };
    let users_unavailable = || notice("users_unavailable", USERS_UNAVAILABLE);
    let Some(db) = db else { return users_unavailable() };
    let Ok(accounts) = subject_accounts(db, agent, employee(), source.subject()) else { return users_unavailable() };
    let state = subject_state(&source, &accounts);
    let mut value = json!({ "state": state.code(), "message": state.message(), "subject": source.subject(),
        "kind": source.kind().as_str() });
    if include_holder
        && let (SubjectState::AttestationRequired | SubjectState::AttestedPerson, [account]) = (state, accounts.as_slice())
    {
        // The person an Admin attests is exactly this holder; unreadable ⇒
        // nothing to attest against.
        let Ok(Some(holder)) = db.google_delegate_holder(&account.user_id) else { return users_unavailable() };
        value["holder"] = json!({ "user_id": holder.user_id, "name": holder.display_name,
            "created_at": holder.created_at, "email_self_set": holder.email_self_set });
    }
    value
}

/// Upper bound of employee directories scanned for a delegate subject.
const MAX_AGENT_SCAN: usize = 4096;

/// SR4 item 2(iv): what a login address `email` now collides with — a
/// designated service mailbox, or an employee's `delegate:<S>` subject. The
/// collision itself stays fail closed at use time; this only names it for the
/// Admin audit. Best effort: unreadable entries are skipped. Blocking reads.
pub(crate) fn email_conflicts(home: &Path, email: &str) -> Vec<serde_json::Value> {
    let Ok(Some(email)) = stored_email(email) else { return Vec::new() };
    let mut conflicts = Vec::new();
    if gi::read_home_config(home).is_ok_and(|config| config.service_mailboxes.contains(&email)) {
        conflicts.push(json!({ "kind": "service_mailbox" }));
    }
    let Ok(entries) = std::fs::read_dir(home.join("agents")) else { return conflicts };
    for entry in entries.flatten().take(MAX_AGENT_SCAN) {
        let Some(agent) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if !duduclaw_core::is_valid_agent_id(&agent) { continue; }
        if let gi::SectionRead::Present(section) = gi::read_agent_section(home, &agent)
            && matches!(&section.identity, Some(gi::Selector::Delegate(subject)) if *subject == email)
        {
            conflicts.push(json!({ "kind": "delegate_subject", "agent_id": agent }));
        }
    }
    conflicts
}

/// SR4 item 2(iv): audit a login email change that collides with a tenant
/// subject (`google_tenant_subject_conflict`, Warning). No conflict ⇒ nothing.
pub(crate) fn audit_email_conflict(home: &Path, user_id: &str, email: &str) {
    let conflicts = email_conflicts(home, email);
    if conflicts.is_empty() { return; }
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        "google_tenant_subject_conflict", "dashboard", duduclaw_security::audit::Severity::Warning,
        json!({ "user_id": user_id, "address": stored_email(email).ok().flatten(), "conflicts": conflicts,
            "effect": "refused_until_resolved" })));
}

/// The tenant source configured for `agent` now, if any. Blocking file
/// reads; call on the same blocking boundary as the other live checks.
/// `Ok(None)`: the employee's Google identity is not a tenant source (home
/// OAuth, Personal, none).
pub(crate) fn read(home: &Path, agent: &str) -> Result<Option<TenantSource>, TenantSourceError> {
    let home_config = gi::read_home_config(home);
    let effective = gi::combine(&gi::read_agent_section(home, agent), &home_config);
    let config = home_config.map_err(|_| TenantSourceError::Unavailable)?;
    let configured_scopes = || -> Result<Vec<String>, TenantSourceError> {
        // Only scopes the tenant mint actually requests; anything else is a
        // configuration error, never silently widened or dropped.
        if config.tenant_scopes.iter().any(|scope| !crate::google_workspace::REQUIRED_SCOPES.contains(&scope.as_str())) {
            return Err(TenantSourceError::Unavailable);
        }
        Ok(config.tenant_scopes.clone())
    };
    match &effective.effective {
        Effective::Invalid(_) => Err(TenantSourceError::Unavailable),
        Effective::Delegate { email } => {
            // A delegate failure must never be served from the home account.
            if effective.fallback != Fallback::None { return Err(TenantSourceError::Refused); }
            let subject = gi::normalize_email(email).map_err(|_| TenantSourceError::Unavailable)?;
            if &subject != email { return Err(TenantSourceError::Unavailable); }
            if !gi::delegate_domain_allowed(&subject, &config, super::home_subject(home).as_deref()) {
                return Err(TenantSourceError::Refused);
            }
            let bytes = read_bounded(&super::delegation_key::path(home), MAX_SEALED_RECORD)?;
            // The record must be the importer's (public part parses, sealed
            // under a protected key id); unsealing stays with the host owner.
            match super::delegation_key::info(home) {
                Ok(Some(_)) => {}
                _ => return Err(TenantSourceError::Unavailable),
            }
            Ok(Some(TenantSource { kind: TenantSourceKind::Delegate, subject,
                service_mailboxes: config.service_mailboxes.clone(), scopes: configured_scopes()?,
                material: material_digest("delegate", &bytes) }))
        }
        Effective::Home => {
            let config_path = home.join("config.toml");
            let raw = match std::fs::symlink_metadata(&config_path) {
                // No config.toml: no service account (home OAuth path).
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(TenantSourceError::Unavailable),
                Ok(_) => read_bounded_or_empty(&config_path, MAX_CONFIG)?,
            };
            let raw = std::str::from_utf8(&raw).map_err(|_| TenantSourceError::Unavailable)?;
            let service_account = match crate::google_service_account::parse_config(raw, home) {
                Ok(None) => return Ok(None),
                Ok(Some(service_account)) => service_account,
                Err(_) => return Err(TenantSourceError::Unavailable),
            };
            // A service-account failure falls back to the Apps Script bridge,
            // a home source: never admissible for a tenant route.
            match crate::google_apps_script::parse_bridge_section(raw) {
                Ok(None) => {}
                _ => return Err(TenantSourceError::Refused),
            }
            let subject = gi::normalize_email(&service_account.subject).map_err(|_| TenantSourceError::Unavailable)?;
            let key = read_bounded(&service_account.key_file, MAX_KEY_FILE)?;
            let text = std::str::from_utf8(&key).map_err(|_| TenantSourceError::Unavailable)?;
            crate::google_service_account::parse_key(text).map_err(|_| TenantSourceError::Unavailable)?;
            Ok(Some(TenantSource { kind: TenantSourceKind::ServiceAccount, subject,
                service_mailboxes: config.service_mailboxes.clone(), scopes: configured_scopes()?,
                material: material_digest("service_account", &key) }))
        }
        Effective::Personal { .. } | Effective::Disabled => Ok(None),
    }
}

fn material_digest(kind: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"duduclaw.e4.google-tenant-source.v1\0");
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A regular, non-empty file of at most `max` bytes. Anything else
/// (missing, a directory, unreadable, oversized, empty) is `Unavailable`.
fn read_bounded(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, TenantSourceError> {
    let bytes = read_bounded_or_empty(path, max)?;
    if bytes.is_empty() { return Err(TenantSourceError::Unavailable); }
    Ok(bytes)
}
fn read_bounded_or_empty(path: &Path, max: u64) -> Result<Zeroizing<Vec<u8>>, TenantSourceError> {
    let unavailable = |_| TenantSourceError::Unavailable;
    let file = std::fs::File::open(path).map_err(unavailable)?;
    let meta = file.metadata().map_err(unavailable)?;
    if !meta.is_file() || meta.len() > max { return Err(TenantSourceError::Unavailable); }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max + 1).read_to_end(&mut bytes).map_err(unavailable)?;
    if bytes.len() as u64 > max { return Err(TenantSourceError::Unavailable); }
    Ok(bytes)
}

// ── F5 UI-RPC: dashboard view of the tenant sources ───────────────────────

/// Domain tag of the dashboard key fingerprint. Distinct from
/// [`material_digest`], so the value shown on the dashboard is never the
/// bridge's pinned source proof.
const FINGERPRINT_DOMAIN: &[u8] = b"duduclaw.f5.google-tenant-key-fingerprint.v1\0";

/// Length-prefixed, domain-separated SHA-256 (hex) of non-secret parts.
fn fingerprint(kind: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

/// The non-secret identity fields of a service-account key file. Serde skips
/// every other field (the private key is never kept in this value).
#[derive(serde::Deserialize)]
struct KeyIdentity {
    client_email: String,
    #[serde(default)]
    private_key_id: Option<String>,
}

/// F5 (UI-RPC): what the dashboard's Google tenant-source tab shows. Holds no
/// secret — the state, identifiers the dashboard already shows elsewhere
/// (service-account email, the SA `subject`) and a key fingerprint:
/// - `delegate` (domain-wide delegation): SHA-256 over the sealed record as
///   stored (ciphertext + public fields); a new key import changes it.
/// - `service_account`: SHA-256 over the key file's `client_email` and its
///   Google key id (`private_key_id`, the id the Google console lists);
///   `null` when the file carries no well-formed key id.
///
/// Each entry is `{"state": "not_configured" | "installed" | "unavailable"}`
/// plus fields when installed. Unreadable or malformed material is
/// `unavailable`, never a guessed fingerprint. Blocking file reads.
pub(crate) fn dashboard_view(home: &Path) -> serde_json::Value {
    json!({ "delegate": delegate_view(home), "service_account": service_account_view(home) })
}

fn delegate_view(home: &Path) -> serde_json::Value {
    let unavailable = json!({ "state": "unavailable" });
    let sealed = super::delegation_key::path(home);
    match std::fs::symlink_metadata(&sealed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return json!({ "state": "not_configured" }),
        Err(_) => return unavailable,
        Ok(_) => {}
    }
    let Ok(bytes) = read_bounded(&sealed, MAX_SEALED_RECORD) else { return unavailable };
    match super::delegation_key::info(home) {
        Ok(Some(info)) => json!({ "state": "installed", "client_email": info.client_email,
            "fingerprint": fingerprint("delegate", &[bytes.as_slice()]) }),
        _ => unavailable,
    }
}

fn service_account_view(home: &Path) -> serde_json::Value {
    let unavailable = json!({ "state": "unavailable" });
    let config_path = home.join("config.toml");
    let raw = match std::fs::symlink_metadata(&config_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return json!({ "state": "not_configured" }),
        Err(_) => return unavailable,
        Ok(_) => match read_bounded_or_empty(&config_path, MAX_CONFIG) { Ok(raw) => raw, Err(_) => return unavailable },
    };
    let Ok(raw) = std::str::from_utf8(&raw) else { return unavailable };
    // `parse_config` reads unparsable TOML as "no section"; the view must not.
    if raw.parse::<toml::Table>().is_err() { return unavailable; }
    let service_account = match crate::google_service_account::parse_config(raw, home) {
        Ok(None) => return json!({ "state": "not_configured" }),
        Ok(Some(service_account)) => service_account,
        Err(_) => return unavailable,
    };
    let subject = gi::normalize_email(&service_account.subject).ok();
    let Ok(key) = read_bounded(&service_account.key_file, MAX_KEY_FILE) else {
        return json!({ "state": "unavailable", "subject": subject });
    };
    let Ok(text) = std::str::from_utf8(&key) else { return json!({ "state": "unavailable", "subject": subject }) };
    if crate::google_service_account::parse_key(text).is_err() {
        return json!({ "state": "unavailable", "subject": subject });
    }
    let Ok(identity) = serde_json::from_str::<KeyIdentity>(text) else {
        return json!({ "state": "unavailable", "subject": subject });
    };
    let key_id = identity.private_key_id.filter(|id| !id.is_empty() && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric()));
    let key_fingerprint = key_id.map(|id| fingerprint("service_account", &[identity.client_email.as_bytes(), id.as_bytes()]));
    json!({
        "state": "installed",
        "client_email": identity.client_email,
        "subject": subject,
        "fingerprint": key_fingerprint,
    })
}

/// Whether any tenant source material is installed in this home at all
/// (C3 provision gate for Google SharedTenant routes). The per-employee
/// subject is decided at use time by [`read`].
pub(crate) fn installed(home: &Path) -> Result<bool, TenantSourceError> {
    let sealed = super::delegation_key::path(home);
    let delegate = match std::fs::symlink_metadata(&sealed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(TenantSourceError::Unavailable),
        Ok(_) => {
            read_bounded(&sealed, MAX_SEALED_RECORD)?;
            match super::delegation_key::info(home) {
                Ok(Some(_)) => true,
                _ => return Err(TenantSourceError::Unavailable),
            }
        }
    };
    if delegate { return Ok(true); }
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(TenantSourceError::Unavailable),
    };
    match crate::google_service_account::parse_config(&raw, home) {
        Ok(None) => Ok(false),
        Ok(Some(service_account)) => {
            read_bounded(&service_account.key_file, MAX_KEY_FILE)?;
            Ok(true)
        }
        Err(_) => Err(TenantSourceError::Unavailable),
    }
}

#[cfg(test)]
#[path = "tenant_source_tests.rs"]
mod tests;
