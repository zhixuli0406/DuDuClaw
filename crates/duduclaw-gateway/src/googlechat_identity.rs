//! Google Chat sender → dashboard account (RM-152).
//!
//! A Chat sender (`message.sender.name`, `users/<id>`) is recognised as a
//! dashboard account through at most two sources, and nothing else:
//!
//! | source | where it lives | removed by |
//! |---|---|---|
//! | administrator binding (always) | `channel_identities` row, channel `googlechat`, verified | the administrator's unbind; deleting the account |
//! | the account's own linked Google account (**opt-in, default off**: `config.toml [google] chat_identity_from_account = true`) | the OpenID `sub` kept in `person_credentials` when the person connected Google in 「我的連線」 | disconnecting Google; suspending the account (record becomes `Suspended`); turning the setting off |
//!
//! The second source rests on an UNVERIFIED assumption (see
//! [`google_sub_matches`]), so it is off unless an administrator turns it on
//! ([`derivation_enabled`]). Only an absent setting reads as off; an
//! unreadable or malformed setting is an error and fails closed (no Google
//! Chat sender is recognised, and approvers are assumed recognisable).
//!
//! There is **no e-mail matching** (decision C-152-Q1 = A). Every lookup
//! failure, ambiguity (two accounts' Google `sub` derive the same id) or
//! disagreement between the two sources means "not recognised": the sender
//! stays an unbound channel user (fail closed). An inactive or deleted
//! account is never recognised — the caller (`trigger_route::access`)
//! re-reads the account and requires it to be active.
//!
//! Trust boundary: a sender id is only meaningful because every inbound Chat
//! event is authenticated before it can start a turn — the shared HTTP app
//! verifies Google's signed request token (`googlechat::webhook_handler` →
//! `googlechat::verify_request`), an employee's own HTTP app does the same
//! (`googlechat_agents::handle`); both then admit only a `HUMAN` sender
//! (`googlechat::is_human_message`) before queueing; the Pub/Sub worker never trusts the
//! notification payload but re-reads the message from the Chat API with the
//! app's own credentials and requires a `HUMAN` `users/…` sender
//! (`googlechat_pubsub::authoritative_event`). Nothing in this module
//! authenticates anything; it must only be fed sender ids from those paths.

use std::path::Path;

use duduclaw_auth::db::UserDb;

/// The `channel_identities.channel` value and session prefix for Google Chat.
pub const CHANNEL: &str = "googlechat";

/// Resource prefix of a Chat user name.
const USER_PREFIX: &str = "users/";

/// Upper bound on the numeric part of an id (Google ids are ~21 digits).
const MAX_ID_LEN: usize = 64;

/// A Chat user id or Google `sub` we are willing to compare: ASCII digits
/// only. This also rejects the `users/app` self-reference and the
/// `users/<email>` alias form the Chat API accepts in requests — an e-mail
/// must never become an identity key.
fn valid_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_digit())
}

/// The ONE place a Chat user id is normalised: `users/<digits>` or bare
/// `<digits>` → `users/<digits>`; anything else → `None`. Exactly one
/// leading `users/` is removed (anchored), the remainder must be all digits,
/// and nothing is trimmed or case-folded — callers handling typed input trim
/// first. Stored bindings and lookups both use this canonical form.
pub fn normalize_user_id(raw: &str) -> Option<String> {
    let id = raw.strip_prefix(USER_PREFIX).unwrap_or(raw);
    valid_numeric_id(id).then(|| format!("{USER_PREFIX}{id}"))
}

/// `config.toml [google]` key that opts in to Google-account-derived
/// recognition. Absent = off.
pub const DERIVATION_KEY: &str = "chat_identity_from_account";

/// Whether Google-account-derived recognition is turned on.
///
/// `Ok(false)` only when the setting is explicitly absent: no `config.toml`
/// (read as an empty file, exactly as the `config_commit` write path does),
/// no `[google]` table, or no `chat_identity_from_account` key. `Ok(true)`
/// only for a literal boolean `true`. Everything else is an `Err`: an I/O
/// error other than not-found, unparsable TOML, a non-table `google`, or a
/// non-boolean value. Callers must treat an `Err` as "cannot decide" and
/// fail closed — never as "off", because "off" also removes derived
/// approvers from `decision_notify::has_recognisable_approver_identity` and
/// would re-open the destination fallback of `authorize_press`.
/// Re-read on every call, so turning it off takes effect on the next message.
pub fn derivation_enabled(home: &Path) -> Result<bool, String> {
    let raw = crate::handlers::config_commit::read_text_or_empty(&home.join("config.toml"))?;
    let table = raw
        .parse::<toml::Table>()
        .map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    let Some(google) = table.get("google") else {
        return Ok(false);
    };
    let google = google
        .as_table()
        .ok_or("config.toml `google` is not a table")?;
    match google.get(DERIVATION_KEY) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("config.toml [google] {DERIVATION_KEY} is not a boolean")),
    }
}

/// Write `[google] chat_identity_from_account` through the same locked,
/// compare-and-swap `config_commit` path as `[google] chat_read`. Returns
/// whether the stored value changed. Callers must be administrators and
/// audit the change (`google.integration.set`).
pub fn set_derivation_enabled(home: &Path, enabled: bool) -> Result<bool, String> {
    let path = home.join("config.toml");
    let raw = crate::handlers::config_commit::read_text_or_empty(&path)?;
    let mut changed = false;
    crate::handlers::config_commit::commit_rendered_locked(
        &path,
        &crate::handlers::config_commit::content_hash(&raw),
        |current| {
            let mut doc: toml_edit::DocumentMut = current
                .parse()
                .map_err(|_| "config.toml 格式錯誤，未變更設定。".to_string())?;
            let google = doc
                .as_table_mut()
                .entry("google")
                .or_insert(toml_edit::table())
                .as_table_mut()
                .ok_or("google 必須是設定表格。")?;
            let previous = match google.get(DERIVATION_KEY) {
                None => false,
                Some(value) => value.as_bool().ok_or("chat_identity_from_account 必須是布林值。")?,
            };
            if previous == enabled {
                return Ok(current.to_owned());
            }
            google.insert(DERIVATION_KEY, toml_edit::value(enabled));
            changed = true;
            Ok(doc.to_string())
        },
    )?;
    Ok(changed)
}

/// Whether a linked Google account's OpenID `sub` is the Chat user
/// `chat_user`.
///
/// ASSUMPTION — NOT VERIFIED AGAINST A REAL GOOGLE WORKSPACE (尚未以真實
/// Google Workspace 驗證): Google documents Chat `users/{user}` as the
/// person's People API / Directory API id, and in practice that is believed
/// to be the same Google account id OpenID Connect returns as `sub`. Nobody
/// has confirmed that the two are one identifier namespace. If they are not,
/// an accidentally equal string would make this return `true` for a Chat
/// sender who is NOT the person who linked that Google account — exact
/// equality only rules out prefix / near-miss matches, not a collision
/// between two namespaces. That is why this comparison is only consulted
/// when an administrator has opted in ([`derivation_enabled`]).
pub fn google_sub_matches(google_sub: &str, chat_user: &str) -> bool {
    match normalize_user_id(chat_user) {
        Some(chat) => valid_numeric_id(google_sub) && chat.strip_prefix(USER_PREFIX) == Some(google_sub),
        None => false,
    }
}

/// A linked Google account record counts as an identity unless the account
/// was suspended (`person_credentials::sync_user_status`). A record that
/// merely needs reconnecting still carries the `sub` Google proved at link
/// time.
fn usable(metadata: &crate::person_credentials::Metadata) -> bool {
    metadata.status != crate::person_credentials::Status::Suspended
}

/// The Chat user a usable linked Google account stands for (list display).
fn chat_user_of(metadata: &crate::person_credentials::Metadata) -> Option<String> {
    if !usable(metadata) || !valid_numeric_id(&metadata.google_sub) {
        return None;
    }
    normalize_user_id(&metadata.google_sub)
}

/// Existing accounts whose own linked Google account derives `chat_user`
/// (canonical). A record left behind by a deleted account is nobody's
/// identity and is skipped. Any read failure — including an unreadable or
/// malformed derivation setting — is an error (fail closed). Empty — without
/// reading the store — while derivation is turned off ([`derivation_enabled`]).
fn google_account_owners(home: &Path, db: &UserDb, chat_user: &str) -> Result<Vec<String>, String> {
    if !derivation_enabled(home)? {
        return Ok(Vec::new());
    }
    let mut owners: Vec<String> = Vec::new();
    for metadata in crate::person_credentials::all_metadata(home)? {
        if !usable(&metadata) || !google_sub_matches(&metadata.google_sub, chat_user) {
            continue;
        }
        if db.get_user(&metadata.owner_id)?.is_none() {
            continue;
        }
        if !owners.contains(&metadata.owner_id) {
            owners.push(metadata.owner_id);
        }
    }
    Ok(owners)
}

/// Combine the two sources. More than one Google owner, or an administrator
/// binding that names someone else than the Google owner, is refused.
fn resolve(admin: Option<String>, google: Vec<String>) -> Result<Option<String>, String> {
    if google.len() > 1 {
        return Err("Google Chat sender matches more than one linked Google account".into());
    }
    match (admin, google.into_iter().next()) {
        (Some(a), Some(g)) if a != g => {
            Err("Google Chat sender's administrator binding and linked Google account name different people".into())
        }
        (Some(a), _) => Ok(Some(a)),
        (None, g) => Ok(g),
    }
}

/// The account a channel sender is bound to through a verified identity.
///
/// Every channel other than Google Chat is answered exactly as before
/// (`find_verified_user_id_by_channel`, unchanged input). For Google Chat
/// the id is normalised, then the administrator binding and (only when
/// [`derivation_enabled`]) the Google-account match are combined by
/// [`resolve`]. The caller must still require the returned account to be
/// active.
///
/// This is THE sender → account mapping. Every channel-side authorization
/// or attribution (decision presses, install sign-off, digest feedback,
/// takeover, decider names, trigger routing) must call this, never
/// `UserDb::find_verified_user_id_by_channel` directly — a direct read
/// silently skips the conflict rules above.
pub fn verified_user_id(
    home: &Path,
    db: &UserDb,
    channel: &str,
    channel_user_id: &str,
) -> Result<Option<String>, String> {
    if channel != CHANNEL {
        return db.find_verified_user_id_by_channel(channel, channel_user_id);
    }
    let Some(chat_user) = normalize_user_id(channel_user_id) else {
        return Ok(None);
    };
    let admin = db.find_verified_user_id_by_channel(CHANNEL, &chat_user)?;
    let google = google_account_owners(home, db, &chat_user)?;
    resolve(admin, google).inspect_err(|e| {
        tracing::warn!(error = %e, "Google Chat sender not recognised (conflicting identities) — treating as unbound");
    })
}

/// Why an administrator may not bind `chat_user` (canonical) to
/// `target_user`: another existing account's own linked Google account
/// already stands for that Chat user. `Ok(None)` = no conflict.
pub fn admin_bind_conflict(
    home: &Path,
    db: &UserDb,
    chat_user: &str,
    target_user: &str,
) -> Result<Option<&'static str>, String> {
    let owners = google_account_owners(home, db, chat_user)?;
    Ok(owners
        .iter()
        .any(|owner| owner != target_user)
        .then_some("這個 Google Chat 使用者已由另一位同仁的 Google 帳號連結認出，不能再綁定給別人。"))
}

/// The Chat user `user_id`'s own linked Google account stands for, with the
/// time it was linked — shown read-only in the dashboard identity list.
/// `None` while derivation is off: the linked Google account is then not an
/// identity and must not be presented as one.
pub fn google_account_chat_user(home: &Path, user_id: &str) -> Result<Option<(String, String)>, String> {
    if !derivation_enabled(home)? {
        return Ok(None);
    }
    Ok(crate::person_credentials::get(home, user_id)?.and_then(|record| {
        chat_user_of(&record.metadata).map(|chat| (chat, record.metadata.connected_at))
    }))
}

/// Whether any of `user_ids` has a usable Google-account-derived Chat
/// identity (decision_notify's "is there a recognisable approver" question).
/// Always `Ok(false)` while derivation is off. Deliberately does not run the
/// per-sender conflict rules: an account that *would* be recognised but for
/// a duplicate owner or a disagreeing administrator binding still counts, so
/// a conflict can only make the role rules apply, never loosen them. A store
/// read failure, or an unreadable / malformed derivation setting, is an `Err`
/// for the caller to treat as "yes" (fail closed).
pub fn any_derived_identity(home: &Path, user_ids: &[&str]) -> Result<bool, String> {
    if !derivation_enabled(home)? {
        return Ok(false);
    }
    Ok(crate::person_credentials::all_metadata(home)?
        .iter()
        .any(|m| user_ids.contains(&m.owner_id.as_str()) && chat_user_of(m).is_some()))
}

#[cfg(test)]
#[path = "googlechat_identity_tests.rs"]
mod tests;

/// Fixtures shared with other modules' regression tests (install sign-off,
/// digest feedback, decision authorization).
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    /// Linked Google accounts as `person_credentials` stores them (owner,
    /// sub, status). Only clear metadata; the sealed secret is a stub.
    pub(crate) fn link_google(home: &Path, records: &[(&str, &str, &str)]) {
        let records: Vec<serde_json::Value> = records
            .iter()
            .enumerate()
            .map(|(i, (owner, sub, status))| {
                serde_json::json!({
                    "owner_kind": "user",
                    "owner_id": owner,
                    "provider": "google",
                    "credential_id": uuid::Uuid::new_v4().to_string(),
                    "revision": 1,
                    "account_email": format!("person{i}@c.test"),
                    "google_sub": sub,
                    "hosted_domain": null,
                    "granted_scopes": ["openid", "email"],
                    "status": status,
                    "connected_at": chrono::Utc::now().to_rfc3339(),
                    "last_refresh_at": null,
                    "oauth_client_id": "client-1",
                    "secret": "pk:v1:stub",
                })
            })
            .collect();
        let file = crate::person_credentials::path(home);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({"version": 1, "records": records, "epochs": {}})).unwrap(),
        )
        .unwrap();
    }

    /// Overwrite `config.toml` with only the derivation setting.
    pub(crate) fn set_derivation(home: &Path, on: bool) {
        std::fs::write(
            home.join("config.toml"),
            format!("[google]\n{} = {on}\n", super::DERIVATION_KEY),
        )
        .unwrap();
    }
}
