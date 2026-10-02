//! Removed-agent name reservation.
//!
//! The MCP `agent_remove` tool does not delete an employee: it moves
//! `<home>/agents/<id>` to `<home>/agents/_trash/<id>_<YYYYmmddHHMMSS>`. Before
//! this module, a supervising AI employee could remove a subordinate and then
//! `create_agent` the same name. The new employee inherits everything keyed by
//! the name (channel bindings, org position, what people expect of it) but none
//! of what the operator configured on the old one — its `CONTRACT.toml`, its
//! `[capabilities]` restrictions, `[container] sandbox_enabled`, … Remove +
//! recreate was a way to strip operator-set controls from a seat.
//!
//! So a name with at least one trash entry is **reserved against AI callers**.
//! Operators (dashboard RPC, a human at a terminal) are not restricted; each
//! entry point decides who its caller is and only calls
//! [`check_name_reserved_for_ai`] for an AI caller.
//!
//! A second, cheaper signal rides along: an `org.toml` record for a name whose
//! directory is gone. Every supported removal path drops that record (MCP
//! `agent_remove`, ephemeral GC), and `org.toml` itself is not AI-writable, so a
//! dangling record means the directory was moved away outside a supported path
//! (`mv agents/x /tmp/x`) — the same seat-takeover shape.

use std::io;
use std::path::Path;

/// Directory under `<home>/agents/` that removed employees are moved into.
pub const AGENT_TRASH_DIR: &str = "_trash";

/// Length of the `%Y%m%d%H%M%S` suffix `agent_remove` appends.
const TRASH_STAMP_DIGITS: usize = 14;

/// The agent id of one `_trash` entry name, or `None` for anything that is not
/// exactly `<id>_<14 ASCII digits>`.
///
/// Anchored on the **trailing** `_` + 14 digits, so an id that itself contains
/// underscores (`sales_east_20261002101010` → `sales_east`) or ends in digits
/// (`team2_20261002101010` → `team2`) parses correctly. Never panics: every
/// slice goes through `str::get`, and the suffix is checked to be ASCII before
/// its position is used.
pub fn trash_entry_agent_id(entry_name: &str) -> Option<&str> {
    let stamp_start = entry_name.len().checked_sub(TRASH_STAMP_DIGITS)?;
    let stamp = entry_name.get(stamp_start..)?;
    if !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = entry_name.get(..stamp_start)?.strip_suffix('_')?;
    (!id.is_empty()).then_some(id)
}

/// Why a name is reserved against an AI caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameReservation {
    /// Nothing reserves the name.
    Free,
    /// `_trash` holds at least one entry for this id.
    RemovedToTrash,
    /// `org.toml` still records this id but its directory is gone.
    DanglingOrgRecord,
    /// `_trash` exists but could not be listed — fail closed.
    Unverifiable,
}

impl NameReservation {
    pub fn is_reserved(self) -> bool {
        !matches!(self, Self::Free)
    }

    /// Stable token for audit rows.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::RemovedToTrash => "removed_to_trash",
            Self::DanglingOrgRecord => "dangling_org_record",
            Self::Unverifiable => "trash_unlistable",
        }
    }
}

/// Whether `<home>/agents/_trash/` holds an entry for `agent_id`.
///
/// `Ok(false)` when the trash directory does not exist. Any other listing
/// error — on the directory or on one entry — is `Err`, so callers can fail
/// closed. Entry names that are not UTF-8 or not in the `<id>_<stamp>` form are
/// ignored. Comparison is whole-id equality (ASCII case-insensitive, because
/// macOS / Windows directory names are), never a prefix or substring test.
pub fn removed_name_in_trash(home: &Path, agent_id: &str) -> io::Result<bool> {
    let trash = home.join("agents").join(AGENT_TRASH_DIR);
    let entries = match std::fs::read_dir(&trash) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if trash_entry_agent_id(name).is_some_and(|id| id.eq_ignore_ascii_case(agent_id)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The reservation verdict for an AI caller creating `agent_id`.
///
/// Callers must have checked already that `<home>/agents/<agent_id>` does not
/// exist (both creation paths refuse an existing directory first); the
/// dangling-record rule relies on that.
pub fn check_name_reserved_for_ai(home: &Path, agent_id: &str) -> NameReservation {
    match removed_name_in_trash(home, agent_id) {
        Ok(true) => return NameReservation::RemovedToTrash,
        Ok(false) => {}
        Err(_) => return NameReservation::Unverifiable,
    }
    if !home.join("agents").join(agent_id).exists() && crate::org_store::load(home).contains(agent_id) {
        return NameReservation::DanglingOrgRecord;
    }
    NameReservation::Free
}

/// The zh-TW refusal shown to an AI caller. No paths, no internal terms.
pub fn name_reserved_message(agent_id: &str, reason: NameReservation) -> String {
    let id = crate::truncate_chars(agent_id, 64);
    match reason {
        NameReservation::Unverifiable => format!(
            "無法建立 AI 員工「{id}」:目前無法確認這個名稱是否屬於先前被移除的員工,\
             為了安全先拒絕。請由管理者在儀表板建立,或改用其他名稱。"
        ),
        _ => format!(
            "無法建立 AI 員工「{id}」:先前有一位同名的員工已被移除,這個名稱目前保留中。\
             要重新使用這個名稱,請由管理者在儀表板處理;或改用其他名稱建立。"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_id() {
        assert_eq!(trash_entry_agent_id("sales_20261002101010"), Some("sales"));
    }

    #[test]
    fn parses_id_with_underscores_and_trailing_digits() {
        assert_eq!(trash_entry_agent_id("sales_east_20261002101010"), Some("sales_east"));
        assert_eq!(trash_entry_agent_id("team2_20261002101010"), Some("team2"));
        assert_eq!(trash_entry_agent_id("a-1_20261002101010"), Some("a-1"));
        // An id that itself looks like an entry still peels exactly one stamp.
        assert_eq!(
            trash_entry_agent_id("x_20261002101010_20261002101011"),
            Some("x_20261002101010")
        );
    }

    #[test]
    fn rejects_junk() {
        for junk in [
            "",
            "sales",
            "_20261002101010",           // empty id
            "sales20261002101010",       // no separator
            "sales_2026100210101",       // 13 digits
            "sales-20261002101010",      // wrong separator
            "sales_2026100210101x",      // non-digit in stamp
            "sales_２０２６１００２１０１０１０", // full-width digits
            "20261002101010",            // stamp only
            "old",
        ] {
            assert_eq!(trash_entry_agent_id(junk), None, "{junk:?}");
        }
        // 15 digits: the trailing 14 parse, the leading digit stays in the id
        // and the separator check fails.
        assert_eq!(trash_entry_agent_id("sales_202610021010101"), None);
    }

    #[test]
    fn multibyte_names_never_panic() {
        for s in ["員工_20261002101010", "員工", "é_2026100210101é", "🐾🐾🐾🐾"] {
            let _ = trash_entry_agent_id(s);
        }
        assert_eq!(trash_entry_agent_id("員工_20261002101010"), Some("員工"));
    }

    fn mk_trash(home: &Path, entries: &[&str]) {
        let t = home.join("agents").join(AGENT_TRASH_DIR);
        std::fs::create_dir_all(&t).unwrap();
        for e in entries {
            std::fs::create_dir_all(t.join(e)).unwrap();
        }
    }

    #[test]
    fn trash_match_is_exact_id_equality() {
        let h = tempfile::tempdir().unwrap();
        mk_trash(h.path(), &["sales_east_20261002101010", "junk", "notes.txt"]);
        assert!(!removed_name_in_trash(h.path(), "sales").unwrap());
        assert!(removed_name_in_trash(h.path(), "sales_east").unwrap());
        assert!(!removed_name_in_trash(h.path(), "east").unwrap());
        assert!(!removed_name_in_trash(h.path(), "junk").unwrap());
    }

    #[test]
    fn missing_trash_is_free() {
        let h = tempfile::tempdir().unwrap();
        assert!(!removed_name_in_trash(h.path(), "sales").unwrap());
        assert_eq!(check_name_reserved_for_ai(h.path(), "sales"), NameReservation::Free);
    }

    #[test]
    fn reserved_verdicts() {
        let h = tempfile::tempdir().unwrap();
        mk_trash(h.path(), &["writer_20261002101010"]);
        assert_eq!(
            check_name_reserved_for_ai(h.path(), "writer"),
            NameReservation::RemovedToTrash
        );
        assert_eq!(check_name_reserved_for_ai(h.path(), "editor"), NameReservation::Free);
    }

    #[test]
    fn dangling_org_record_is_reserved() {
        let h = tempfile::tempdir().unwrap();
        crate::org_store::upsert(h.path(), "ghost", crate::OrgEntry::new("ceo", "")).unwrap();
        assert_eq!(
            check_name_reserved_for_ai(h.path(), "ghost"),
            NameReservation::DanglingOrgRecord
        );
    }

    #[test]
    fn trash_that_is_not_a_directory_fails_closed() {
        // `_trash` exists but cannot be listed (a regular file here; a
        // permission error behaves the same way).
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(h.path().join("agents")).unwrap();
        std::fs::write(h.path().join("agents").join(AGENT_TRASH_DIR), "x").unwrap();
        assert!(removed_name_in_trash(h.path(), "sales").is_err());
        assert_eq!(
            check_name_reserved_for_ai(h.path(), "sales"),
            NameReservation::Unverifiable
        );
    }

    #[test]
    fn messages_carry_no_paths() {
        for r in [NameReservation::RemovedToTrash, NameReservation::Unverifiable] {
            let m = name_reserved_message("writer", r);
            assert!(m.contains("writer"));
            assert!(m.contains("儀表板"));
            assert!(!m.contains('/') && !m.contains("_trash") && !m.contains("rm "), "{m}");
        }
    }
}
