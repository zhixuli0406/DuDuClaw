//! Workspace lifecycle states (design §3.5). Leases are separate columns and
//! never change the state.

/// One workspace's state. Stored as its [`Self::as_str`] spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Row inserted, directory not yet made.
    Creating,
    /// Usable: attach, read, write.
    Ready,
    /// Retention ran out. Listed and readable, never attached or written.
    Expired,
    /// An operator suspended it. Status only; content is not readable.
    Revoked,
    /// Its owner was removed. Only an operator can deal with it.
    Orphaned,
    /// An operator delete is in progress.
    Deleting,
    /// Tombstone; the id is never reused.
    Deleted,
    /// Making the directory failed; reconciled at boot.
    FailedCreate,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::Orphaned => "orphaned",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
            Self::FailedCreate => "failed_create",
        }
    }

    /// Parse a stored value. Unknown text is `None` (callers fail closed).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "creating" => Self::Creating,
            "ready" => Self::Ready,
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            "orphaned" => Self::Orphaned,
            "deleting" => Self::Deleting,
            "deleted" => Self::Deleted,
            "failed_create" => Self::FailedCreate,
            _ => return None,
        })
    }

    /// Whether the owner may read file content (D6: not when revoked).
    pub fn content_readable(self) -> bool {
        matches!(self, Self::Ready | Self::Expired)
    }

    /// Whether an operator delete may start from here.
    pub fn deletable(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Expired | Self::Revoked | Self::Orphaned | Self::FailedCreate
        )
    }

    /// Whether an operator revoke may apply (any non-deletion state).
    pub fn revocable(self) -> bool {
        matches!(self, Self::Ready | Self::Expired | Self::Orphaned)
    }

    /// Owner-facing explanation of a state that blocks attach / write.
    pub fn blocked_message(self) -> &'static str {
        match self {
            Self::Ready => "工作區可以使用。",
            Self::Expired => {
                "這個工作區已超過保留期限（資料仍在、可以讀取），不能再掛載或寫入；請管理員延長保留（renew）或刪除。"
            }
            Self::Revoked => "這個工作區已被管理員暫停授權，目前不能使用。",
            Self::Orphaned => "這個工作區的原主人已被移除，只有管理員能處理。",
            Self::Deleting | Self::Deleted => "這個工作區已被刪除。",
            Self::Creating | Self::FailedCreate => "這個工作區尚未建立完成，目前不能使用。",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips() {
        for s in [
            WorkspaceState::Creating,
            WorkspaceState::Ready,
            WorkspaceState::Expired,
            WorkspaceState::Revoked,
            WorkspaceState::Orphaned,
            WorkspaceState::Deleting,
            WorkspaceState::Deleted,
            WorkspaceState::FailedCreate,
        ] {
            assert_eq!(WorkspaceState::parse(s.as_str()), Some(s));
        }
        assert_eq!(WorkspaceState::parse("READY"), None);
    }

    #[test]
    fn revoked_content_is_not_readable() {
        assert!(WorkspaceState::Ready.content_readable());
        assert!(WorkspaceState::Expired.content_readable());
        assert!(!WorkspaceState::Revoked.content_readable());
        assert!(!WorkspaceState::Orphaned.content_readable());
    }
}
