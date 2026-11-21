//! Who may, through an employee, use the person's Google account
//! (`[google] requesters`, design §5.2). Only person identities are gated;
//! `home` is unchanged.
//!
//! | `requesters` | allowed |
//! |---|---|
//! | `bound_people` (default) | a signed-in or verified-channel person who is bound to the employee (any access level) or an admin; a system turn on behalf of nobody (cron, heartbeat, Agent Mail); a delegated run only when the upstream person would be allowed |
//! | `owner_only` | the account's own person (the users.db account with the delegate's email) and system turns on behalf of nobody |
//! | `anyone` | everything except public visitors |
//!
//! Public visitors (`widget`, `odoolivechat`) are refused under every
//! setting. Under `bound_people` / `owner_only` an unbound channel sender
//! and an unknown turn are refused. People are re-read on every call, so a
//! suspended account stops counting at once.

use std::sync::Arc;

use duduclaw_auth::db::UserDb;
use duduclaw_knowledge::requester::{RequesterIdentity, is_public_visitor_channel};

use super::{Refusal, Requesters};

/// One account, as far as this gate cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub user_id: String,
    pub email: String,
    pub active: bool,
    pub admin: bool,
}

/// The account directory the gate reads. A lookup failure is "nobody".
pub trait People: Send + Sync {
    fn user_db(&self) -> Option<&UserDb> { None }
    fn by_id(&self, user_id: &str) -> Option<Person>;
    fn by_email(&self, email: &str) -> Result<Option<Person>, Refusal>;
    fn bound(&self, user_id: &str, agent: &str) -> bool;
}

/// `users.db` behind the gate.
pub struct UserDbPeople(pub Arc<UserDb>);

fn person(u: duduclaw_auth::models::User) -> Person {
    Person {
        active: u.status == duduclaw_auth::models::UserStatus::Active,
        admin: u.role == duduclaw_auth::models::UserRole::Admin,
        user_id: u.id,
        email: u.email,
    }
}

impl People for UserDbPeople {
    fn user_db(&self) -> Option<&UserDb> { Some(self.0.as_ref()) }
    fn by_id(&self, user_id: &str) -> Option<Person> {
        self.0.get_user(user_id).ok().flatten().map(person)
    }
    fn by_email(&self, email: &str) -> Result<Option<Person>, Refusal> {
        // Whole-address, case-insensitive equality (stored addresses may
        // keep the case they were typed with).
        Ok(self
            .0
            .list_users()
            .map_err(|_| directory_unavailable())?
            .into_iter()
            .find(|u| u.email.trim().eq_ignore_ascii_case(email.trim()))
            .map(person))
    }
    fn bound(&self, user_id: &str, agent: &str) -> bool {
        self.0
            .get_agent_users(agent)
            .map(|rows| rows.iter().any(|b| b.user_id == user_id))
            .unwrap_or(false)
    }
}

/// No account database: knows nobody.
pub struct NoPeople;

impl People for NoPeople {
    fn by_id(&self, _: &str) -> Option<Person> {
        None
    }
    fn by_email(&self, _: &str) -> Result<Option<Person>, Refusal> {
        Ok(None)
    }
    fn bound(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn directory_unavailable() -> Refusal {
    Refusal::hard(
        "config_invalid",
        "無法確認同事帳號的狀態，已停止使用公司授權代表。請管理員確認人員資料可以讀取。",
    )
}

struct UnavailablePeople;

impl People for UnavailablePeople {
    fn by_id(&self, _: &str) -> Option<Person> {
        None
    }
    fn by_email(&self, _: &str) -> Result<Option<Person>, Refusal> {
        Err(directory_unavailable())
    }
    fn bound(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// The directory for `home`: `users` when already open, else `users.db`
/// read through (never created); none or unreadable knows nobody.
pub fn people_for(home: &std::path::Path, users: Option<&Arc<UserDb>>) -> Box<dyn People> {
    match users {
        Some(db) => Box::new(UserDbPeople(db.clone())),
        None => match crate::decision_notify::open_user_db(home) {
            Ok(Some(db)) => Box::new(UserDbPeople(Arc::new(db))),
            Ok(None) => Box::new(NoPeople),
            Err(_) => Box::new(UnavailablePeople),
        },
    }
}

const REFUSED: &str = "這個對話的發問者沒有權限透過這位 AI 員工使用 Google 帳號。";

/// The next step for a Google Chat sender the gate cannot recognise (no
/// verified binding). The raw Chat id is never printed: in a space every
/// member would see it.
pub(crate) const GOOGLECHAT_BIND_NEXT_STEP: &str =
    "請管理員在儀表板「使用者」頁把你的 Google Chat 帳號綁定到你的同事帳號。";

fn refused() -> Refusal {
    Refusal::hard("requester_refused", REFUSED)
}

/// [`refused`] with the next step a refused requester can act on.
fn refused_for(requester: &RequesterIdentity) -> Refusal {
    match requester {
        RequesterIdentity::ChannelUser { channel, user_id: None, .. } if channel == "googlechat" => {
            Refusal::hard("requester_refused", format!("{REFUSED}{GOOGLECHAT_BIND_NEXT_STEP}"))
        }
        _ => refused(),
    }
}

/// The person a requester stands for, if any.
fn person_of(req: &RequesterIdentity) -> Option<&str> {
    match req {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser {
            user_id: Some(user_id),
            ..
        }
        | RequesterIdentity::System {
            on_behalf_of: Some(user_id),
            ..
        } => Some(user_id.as_str()),
        _ => None,
    }
}

/// Whether `requester` stands for exactly the DuDuClaw account `user_id`
/// (exact id equality; an unbound sender or a system turn on behalf of
/// nobody is never that person).
pub fn is_person(requester: &RequesterIdentity, user_id: &str) -> bool {
    !user_id.is_empty() && person_of(requester) == Some(user_id)
}

/// Decide whether `requester` may use `account_email` through `agent`.
pub fn check(
    requesters: Requesters,
    requester: &RequesterIdentity,
    agent: &str,
    account_email: &str,
    people: &dyn People,
) -> Result<(), Refusal> {
    if let RequesterIdentity::ChannelUser { channel, .. } = requester
        && is_public_visitor_channel(channel)
    {
        return Err(refused());
    }
    if requesters == Requesters::Anyone {
        return Ok(());
    }
    if matches!(
        requester,
        RequesterIdentity::System {
            on_behalf_of: None,
            ..
        }
    ) {
        return Ok(());
    }
    let Some(user_id) = person_of(requester) else {
        return Err(refused_for(requester));
    };
    let Some(p) = people.by_id(user_id).filter(|p| p.active) else {
        return Err(refused());
    };
    let allowed = match requesters {
        Requesters::OwnerOnly => people
            .by_email(account_email)?
            .is_some_and(|owner| owner.active && owner.user_id == p.user_id),
        _ => p.admin || people.bound(&p.user_id, agent),
    };
    if allowed { Ok(()) } else { Err(refused()) }
}

#[cfg(test)]
mod next_step_tests {
    use super::*;

    fn sender(channel: &str, chat: &str, user_id: Option<&str>) -> RequesterIdentity {
        RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: chat.into(),
            sender: "users/1234567890".into(),
            user_id: user_id.map(str::to_string),
            attested_private: false,
        }
    }

    #[test]
    fn unbound_google_chat_sender_is_told_how_to_get_bound_without_the_raw_id() {
        let error = check(Requesters::BoundPeople, &sender("googlechat", "spaces/A", None), "a", "", &NoPeople)
            .unwrap_err();
        assert_eq!(error.code, "requester_refused");
        assert!(error.message_zh.starts_with(REFUSED));
        assert!(error.message_zh.ends_with(GOOGLECHAT_BIND_NEXT_STEP));
        assert!(!error.message_zh.contains("users/"), "{}", error.message_zh);
        assert!(!error.message_zh.contains("1234567890"));
    }

    #[test]
    fn other_refusals_keep_their_text() {
        for requester in [
            sender("line", "U1", None),
            sender("widget", "w", None),
            sender("googlechat", "spaces/A", Some("missing-user")),
            RequesterIdentity::Unknown,
        ] {
            let error = check(Requesters::BoundPeople, &requester, "a", "", &NoPeople).unwrap_err();
            assert_eq!(error.message_zh, REFUSED, "{requester:?}");
        }
    }

    #[test]
    fn is_person_is_exact() {
        let alice = sender("googlechat", "spaces/A", Some("alice"));
        assert!(is_person(&alice, "alice"));
        assert!(!is_person(&alice, "alic"));
        assert!(!is_person(&alice, ""));
        assert!(!is_person(&sender("googlechat", "spaces/A", None), "alice"));
        assert!(!is_person(
            &RequesterIdentity::System { kind: "cron".into(), on_behalf_of: None },
            "alice"
        ));
    }
}
