//! Host-issued proof that a person accepted the current Google data-use
//! disclosure.  Caller parameters can acknowledge the UI, but cannot choose
//! the revision, timestamp, or actor persisted with a credential.

use serde::{Deserialize, Serialize};

pub(crate) const CURRENT_REVISION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub(crate) revision: u32,
    pub(crate) accepted_at: String,
    pub(crate) actor: String,
}

impl Evidence {
    pub(crate) fn issue(actor: &str) -> Result<Self, String> {
        if actor.is_empty()
            || actor.len() > 128
            || !actor
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("無法確認 Google 資料同意者。".into());
        }
        Ok(Self {
            revision: CURRENT_REVISION,
            accepted_at: chrono::Utc::now().to_rfc3339(),
            actor: actor.to_owned(),
        })
    }

    pub(crate) fn is_well_formed(&self) -> bool {
        self.revision > 0
            && !self.actor.is_empty()
            && self.actor.len() <= 128
            && self
                .actor
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && chrono::DateTime::parse_from_rfc3339(&self.accepted_at).is_ok()
    }

    pub(crate) fn is_current(&self) -> bool {
        self.revision == CURRENT_REVISION && self.is_well_formed()
    }
}

pub(crate) fn require_current(value: Option<&Evidence>) -> Result<(), String> {
    if value.is_some_and(Evidence::is_current) {
        Ok(())
    } else {
        Err("Google 資料使用說明已更新或尚未確認；請重新連結 Google。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_issued_current_evidence_is_accepted() {
        let current = Evidence::issue("admin_1").unwrap();
        assert!(current.is_current());
        assert!(require_current(Some(&current)).is_ok());
        assert!(require_current(None).is_err());
        let mut stale = current.clone();
        stale.revision = CURRENT_REVISION + 1;
        assert!(stale.is_well_formed());
        assert!(require_current(Some(&stale)).is_err());
        let mut old = current;
        old.revision = 0;
        assert!(!old.is_well_formed());
        assert!(require_current(Some(&old)).is_err());
    }
}
