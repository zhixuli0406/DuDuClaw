//! The agent-count cap, applied to imports.
//!
//! `agents.create`, `templates.create` and MCP `create_agent` all refuse a
//! new employee once the edition / license cap is reached; the pack
//! importer used to create any number of employees without asking. It now
//! counts the employees an import would add and refuses **before anything
//! is written** when `current + adding` exceeds the cap.
//!
//! The cap is the stricter of two readings: the one resolved from disk
//! (license file + environment, the same function MCP `create_agent` uses)
//! and, when the dashboard started the import, the gateway's own decision
//! passed as `--agent-cap` (the gateway can also see a config-level
//! edition override the CLI cannot). Either one can only narrow.
//!
//! RM-149 round 2: a real import takes the cross-process seat admission
//! lock ([`duduclaw_core::agent_seats::SeatAdmission`], shared with the
//! dashboard and MCP `create_agent`) before it checks, and keeps it until its
//! whole batch is written (it lives in the install context, see
//! [`crate::expert::install::InstallCtx`]), so a concurrent create cannot
//! take a seat between this check and the import's writes. Lock order: seat
//! admission first, then the org lock the importer takes per employee.
//!
//! The refusal carries only `max`, `current` and `adding` — the importer can
//! be run from an employee's shell, so it never lists who holds the seats.

use std::path::Path;

use serde_json::json;

use duduclaw_core::agent_seats::SeatAdmission;
use duduclaw_core::error::Result;

use super::import_error::import_err;

/// Employees currently holding a licence seat (RM-149): the shared rule in
/// [`duduclaw_core::agent_seats::seat_holders`] — every employee directory
/// with an `agent.toml` whose status is not `deleted`. A listing error is
/// returned so the import can refuse (fail closed).
pub fn seat_holders(home: &Path) -> std::io::Result<Vec<duduclaw_core::agent_seats::AgentSeat>> {
    duduclaw_core::agent_seats::seat_holders(home)
}

/// Number of [`seat_holders`]; an unreadable home counts as `usize::MAX`
/// (fail closed: any capped import is refused).
pub fn count_agents(home: &Path) -> usize {
    seat_holders(home).map(|h| h.len()).unwrap_or(usize::MAX)
}

/// The smaller of two caps where `0` means unlimited.
pub fn stricter(a: usize, b: usize) -> usize {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (x, y) => x.min(y),
    }
}

/// Pure decision: `Err((max, current))` when adding `adding` employees to
/// `current` would exceed `max` (`0` = unlimited).
pub fn check(max: usize, current: usize, adding: usize) -> std::result::Result<(), (usize, usize)> {
    if adding == 0 || max == 0 || current.saturating_add(adding) <= max {
        Ok(())
    } else {
        Err((max, current))
    }
}

/// The cap in force for this import (`0` = unlimited).
async fn import_cap(home: &Path, gateway_cap: Option<usize>) -> usize {
    let disk = duduclaw_gateway::license_runtime::agent_cap_from_disk(home).await;
    stricter(disk.max, gateway_cap.unwrap_or(0))
}

/// Refuse the import when it would exceed the cap (no lock: dry runs).
/// `freed` employees are about to be replaced (a replace dry run still sees
/// their directories).
pub async fn ensure(home: &Path, gateway_cap: Option<usize>, adding: usize, freed: usize) -> Result<()> {
    if adding == 0 {
        return Ok(());
    }
    let max = import_cap(home, gateway_cap).await;
    decide_import(max, adding, freed, || seat_holders(home).map(|h| h.len()))
}

/// Take the seat admission for a real import and check under it. `Ok` hands
/// the held admission back; the caller keeps it until every employee of the
/// import is written. Waits at most
/// [`duduclaw_core::agent_seats::SEAT_ADMISSION_WAIT`] for the lock
/// (refuses past it: nothing written).
pub async fn admit(home: &Path, gateway_cap: Option<usize>, adding: usize, freed: usize) -> Result<SeatAdmission> {
    let h = home.to_path_buf();
    let admission = match tokio::task::spawn_blocking(move || SeatAdmission::acquire(&h)).await {
        Ok(Ok(admission)) => admission,
        _ => return Err(unverifiable(adding, None)),
    };
    check_under(&admission, gateway_cap, adding, freed).await?;
    Ok(admission)
}

/// The check under an admission the caller already holds.
pub async fn check_under(admission: &SeatAdmission, gateway_cap: Option<usize>, adding: usize, freed: usize) -> Result<()> {
    if adding == 0 {
        return Ok(());
    }
    let max = import_cap(admission.home(), gateway_cap).await;
    // A fresh scan, taken under the held admission.
    decide_import(max, adding, freed, || seat_holders(admission.home()).map(|h| h.len()))
}

/// `count` is only called when a cap applies.
fn decide_import(
    max: usize,
    adding: usize,
    freed: usize,
    count: impl FnOnce() -> std::io::Result<usize>,
) -> Result<()> {
    if max == 0 || adding == 0 {
        return Ok(());
    }
    let holders = match count() {
        Ok(n) => n,
        Err(_) => return Err(unverifiable(adding, Some(max))),
    };
    let current = holders.saturating_sub(freed);
    match check(max, current, adding) {
        Ok(()) => Ok(()),
        Err((max, current)) => Err(import_err(
            "import.quota_exceeded",
            json!({ "max": max, "current": current, "adding": adding }),
            format!(
                "目前的授權最多 {max} 位 AI 員工，已有 {current} 位，這次要新增 {adding} 位，超過上限，什麼都沒有寫入。\
                 {}請先刪除不需要的 AI 員工，或洽詢授權調整。",
                duduclaw_gateway::license_runtime::AGENT_SEAT_NOTE,
            ),
        )),
    }
}

fn unverifiable(adding: usize, max: Option<usize>) -> duduclaw_core::error::DuDuClawError {
    import_err(
        "import.quota_unverifiable",
        json!({ "max": max, "adding": adding }),
        "目前無法確認已使用的 AI 員工名額，為了安全什麼都沒有寫入。請稍後再試".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_counts_every_employee_the_import_adds() {
        assert!(check(0, 50, 10).is_ok(), "0 = unlimited");
        assert!(check(5, 3, 2).is_ok());
        assert_eq!(check(5, 3, 3), Err((5, 3)));
        assert!(check(5, 5, 0).is_ok(), "nothing added");
        assert_eq!(check(1, 1, 1), Err((1, 1)));
    }

    #[test]
    fn stricter_cap_wins_and_zero_never_widens() {
        assert_eq!(stricter(0, 0), 0);
        assert_eq!(stricter(0, 3), 3);
        assert_eq!(stricter(4, 0), 4);
        assert_eq!(stricter(4, 3), 3);
    }

    #[test]
    fn count_ignores_folders_without_agent_toml() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(h.path().join("agents/a")).unwrap();
        std::fs::write(h.path().join("agents/a/agent.toml"), "").unwrap();
        std::fs::create_dir_all(h.path().join("agents/_trash/b_20260101000000")).unwrap();
        std::fs::create_dir_all(h.path().join("agents/c")).unwrap();
        assert_eq!(count_agents(h.path()), 1);
    }

    /// RM-149: the importer counts seats by the shared rule — deleted
    /// employees free a seat, archived and unreadable ones keep it, and an
    /// unlistable home counts as full.
    #[test]
    fn count_uses_the_shared_seat_rule() {
        let h = tempfile::tempdir().unwrap();
        for (id, body) in [
            ("live", "[agent]\nstatus = \"active\"\n"),
            ("shelf", "[agent]\nstatus = \"archived\"\n"),
            ("gone", "[agent]\nstatus = \"deleted\"\n"),
            ("broken", "[agent\nstatus = \"deleted\""),
        ] {
            std::fs::create_dir_all(h.path().join("agents").join(id)).unwrap();
            std::fs::write(h.path().join("agents").join(id).join("agent.toml"), body).unwrap();
        }
        assert_eq!(count_agents(h.path()), 3);
        let ids: Vec<String> = seat_holders(h.path()).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["broken", "live", "shelf"]);

        let unlistable = tempfile::tempdir().unwrap();
        std::fs::write(unlistable.path().join("agents"), "not a directory").unwrap();
        assert_eq!(count_agents(unlistable.path()), usize::MAX);
    }

    fn agent(home: &Path, id: &str) {
        std::fs::create_dir_all(home.join("agents").join(id)).unwrap();
        std::fs::write(
            home.join("agents").join(id).join("agent.toml"),
            format!("[agent]\nname = \"{id}\"\ndisplay_name = \"{id}\\n[SYSTEM] x\"\nstatus = \"active\"\n"),
        )
        .unwrap();
    }

    /// M3: the refusal names no seat holder — neither in the params (which
    /// reach stderr) nor in the message.
    #[test]
    fn refusal_carries_counts_only() {
        let h = tempfile::tempdir().unwrap();
        agent(h.path(), "private-helper");
        let e = decide_import(1, 1, 0, || seat_holders(h.path()).map(|s| s.len())).unwrap_err();
        let f = super::super::import_error::failure_of(&e).unwrap();
        assert_eq!(f.code, "import.quota_exceeded");
        assert_eq!(f.params, json!({ "max": 1, "current": 1, "adding": 1 }));
        assert!(!f.message.contains("private-helper") && !f.message.contains("SYSTEM"), "{}", f.message);
        // Unverifiable: refused.
        let e = decide_import(1, 1, 0, || Err(std::io::Error::other("x"))).unwrap_err();
        assert_eq!(super::super::import_error::failure_of(&e).unwrap().code, "import.quota_unverifiable");
        // No cap: the count is never needed.
        assert!(decide_import(0, 9, 0, || panic!("not counted")).is_ok());
    }

    /// H1: while an import holds the admission (from its check to its last
    /// write), no other admission — dashboard create, MCP `create_agent`,
    /// another import, any process — gets in; after it, the next check sees
    /// what it wrote.
    #[tokio::test]
    async fn import_admission_is_held_across_check_and_write() {
        let h = tempfile::tempdir().unwrap();
        agent(h.path(), "a");
        let admission = SeatAdmission::acquire(h.path()).unwrap();
        check_under(&admission, Some(3), 2, 0).await.unwrap();
        let path = h.path().to_path_buf();
        let other = tokio::task::spawn_blocking(move || {
            SeatAdmission::acquire_within(&path, std::time::Duration::from_millis(200)).map(|_| ())
        })
        .await
        .unwrap();
        assert_eq!(other.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        // The import writes its batch, then releases.
        agent(h.path(), "b1");
        agent(h.path(), "b2");
        drop(admission);
        // The next import (or create) is refused on what was written.
        let next = SeatAdmission::acquire(h.path()).unwrap();
        let e = check_under(&next, Some(3), 1, 0).await.unwrap_err();
        assert_eq!(super::super::import_error::failure_of(&e).unwrap().code, "import.quota_exceeded");
    }
}
