//! Non-admin changes can choose the caller's own account and only narrow
//! the fallback/requester boundary. Generic agent updates stay narrower;
//! this dedicated flow records self-consent separately.
use duduclaw_core::google_identity::{AgentGoogleSection, Fallback, Selector};

pub fn validate(
    before: &AgentGoogleSection,
    after: &AgentGoogleSection,
    caller: &str,
    selected_person: Option<&str>,
    confirmed: bool,
) -> Result<bool, String> {
    if after.identity == Some(Selector::None) {
        return Ok(false);
    }
    let own = match &after.identity {
        Some(Selector::User(user)) => user == caller,
        Some(Selector::Owner) => selected_person == Some(caller),
        _ => false,
    };
    if !own && after.identity != Some(Selector::None) {
        return Err("你只能選擇自己的 Google 帳號，或關閉 Google。其他變更請聯絡管理員。".into());
    }
    if (after.fallback != before.fallback && after.fallback != Fallback::None)
        || after.requesters < before.requesters
    {
        return Err("只有管理員能擴大使用者範圍，或開啟改用公司帳號。".into());
    }
    if own && !confirmed {
        return Err("請先確認哪些人能透過這位員工使用你的 Google 帳號。".into());
    }
    Ok(own)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::google_identity::Requesters;
    #[test]
    fn self_selection_requires_confirmation_and_cannot_widen_the_boundary() {
        let before = AgentGoogleSection {
            identity: Some(Selector::Home),
            fallback: Fallback::None,
            requesters: Requesters::OwnerOnly,
        };
        let mut after = before.clone();
        after.identity = Some(Selector::User("alice".into()));
        assert!(validate(&before, &after, "alice", Some("alice"), false).is_err());
        assert_eq!(
            validate(&before, &after, "alice", Some("alice"), true).unwrap(),
            true
        );
        assert!(validate(&before, &after, "bob", Some("alice"), true).is_err());
        after.requesters = Requesters::BoundPeople;
        assert!(validate(&before, &after, "alice", Some("alice"), true).is_err());
        after.requesters = Requesters::OwnerOnly;
        after.fallback = Fallback::Home;
        assert!(validate(&before, &after, "alice", Some("alice"), true).is_err());
        after.fallback = Fallback::None;
        after.identity = Some(Selector::Owner);
        assert!(validate(&before, &after, "alice", Some("bob"), true).is_err());
        after.identity = Some(Selector::None);
        assert_eq!(
            validate(&before, &after, "alice", None, false).unwrap(),
            false
        );
    }
}
