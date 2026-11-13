//! Licence seats: which AI employees count against the agent cap (RM-149).
//!
//! Every place that compares "how many employees exist" with the edition or
//! licence cap (`agents.create`, `templates.create_agent`, `templates.apply`,
//! `agents.unarchive`, MCP `create_agent`, the pack importer) asks
//! [`seat_holders`]. Nobody counts on their own.
//!
//! The rule: an employee directory under `<home>/agents/` with an
//! `agent.toml` holds a seat unless its `[agent] status` is exactly
//! `"deleted"`. Archived employees still hold one (they can be restored at
//! any time). The dashboard soft-delete keeps the directory on disk, so
//! deleted employees are still found here, reported, and skipped.
//!
//! Fail closed: an `agent.toml` that cannot be read or parsed, or whose
//! status is not a string, counts as a seat holder with status
//! [`UNREADABLE_STATUS`]. Only a status this module can read as `"deleted"`
//! frees a seat.
//!
//! The scan reads the files on disk, not the gateway's in-memory registry:
//! the registry leaves out deleted employees and anything that fails to load,
//! so counting it would undercount exactly the broken files that must count.
//!
//! # Admission (check and create are one step)
//!
//! Counting alone is not enough: two creates that both count 4 of 5 seats
//! and then both publish end at 6. Every entry point that adds a seat holder
//! (the six above) therefore takes the one cross-process **seat admission
//! lock** ([`SeatAdmission`], `<home>/locks/agent-seat-admission`) and, while
//! holding it, resolves the cap, re-scans the seats ([`decide`] /
//! [`SeatAdmission::decide`]) and publishes the new employee directory — or
//! the whole batch, for a team template or a pack import — before releasing
//! it. A batch is admitted all-or-nothing: either all N fit under the one
//! held lock or none is written.
//!
//! Lock order, everywhere: the seat admission lock is taken **first** —
//! before the global org mutation lock
//! ([`crate::org_store::lock_org_mutations`]), any `agent.toml` lock and the
//! autonomy lifecycle barrier. Never take it while holding one of those. It
//! is not re-entrant: a batch takes it once and passes it down. Every wait on
//! it is bounded by [`SEAT_ADMISSION_WAIT`]; a caller that cannot get it
//! refuses (fail closed).
//!
//! Reports: display names and statuses copied out of `agent.toml` are
//! untrusted text. They are reduced to one plain line
//! ([`crate::plain_line`]: no control, newline or bidi characters, at most
//! [`MAX_REPORT_CHARS`] characters) before they leave this module.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The `[agent] status` value of a soft-deleted employee (the only status
/// that frees a seat).
pub const DELETED_STATUS: &str = "deleted";
/// The `[agent] status` value of an archived (restorable) employee.
pub const ARCHIVED_STATUS: &str = "archived";
/// Reported status when `agent.toml` (or its status) could not be read.
pub const UNREADABLE_STATUS: &str = "unreadable";
/// Reported status when `agent.toml` parses but has no `[agent] status`.
pub const UNKNOWN_STATUS: &str = "unknown";

/// Stable `error.code` of a create / restore refused by the seat limit. The
/// dashboard keys its seat-holder list off it.
pub const SEAT_LIMIT_ERROR_CODE: &str = "agent_seat_limit_reached";

/// Longest id / display name / status copied into a report (characters).
pub const MAX_REPORT_CHARS: usize = 64;

/// File name of the seat admission lock under `<home>/locks/` (the advisory
/// lock itself lives on `<that>.lock`).
pub const SEAT_ADMISSION_LOCK: &str = "agent-seat-admission";

/// Longest wait for the seat admission lock. A create holds it for the time
/// it takes to write one employee directory (a pack import: its whole
/// batch); past this the caller refuses rather than wait unboundedly.
pub const SEAT_ADMISSION_WAIT: Duration = Duration::from_secs(30);

/// One employee directory and whether it holds a licence seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSeat {
    /// Directory name under `<home>/agents/` as found on disk. Used for
    /// matching only; reports pass it through [`crate::plain_line`].
    pub id: String,
    /// `[agent] display_name`, else `[agent] name`, else the id — one plain
    /// line of at most [`MAX_REPORT_CHARS`] characters.
    pub display_name: String,
    /// `[agent] status` as one plain line of at most [`MAX_REPORT_CHARS`]
    /// characters, or [`UNREADABLE_STATUS`] / [`UNKNOWN_STATUS`]. For display
    /// only: the decisions below use the raw value.
    pub status: String,
    /// `[agent] name` when readable (raw; the registry key).
    pub name: Option<String>,
    /// Decided from the full, raw status.
    holds_seat: bool,
    /// Decided from the full, raw status.
    archived: bool,
}

impl AgentSeat {
    fn unreadable(id: String) -> Self {
        Self {
            display_name: crate::plain_line(&id, MAX_REPORT_CHARS),
            id,
            status: UNREADABLE_STATUS.to_string(),
            name: None,
            holds_seat: true,
            archived: false,
        }
    }

    /// Whether this employee counts against the licence.
    pub fn holds_seat(&self) -> bool {
        self.holds_seat
    }

    /// Whether the raw status is exactly `"archived"`.
    pub fn is_archived(&self) -> bool {
        self.archived
    }

    /// Whether the raw status is exactly `"deleted"`.
    pub fn is_deleted(&self) -> bool {
        !self.holds_seat
    }
}

/// The seat entry for one `agent.toml` text. Pure.
pub fn parse_seat(id: &str, agent_toml: &str) -> AgentSeat {
    let Ok(table) = agent_toml.parse::<toml::Table>() else {
        return AgentSeat::unreadable(id.to_string());
    };
    let agent = table.get("agent").and_then(|v| v.as_table());
    let text = |key: &str| {
        agent
            .and_then(|a| a.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let name = text("name");
    let (status, holds_seat, archived) = match agent.and_then(|a| a.get("status")) {
        Some(toml::Value::String(s)) => (
            crate::plain_line(s, MAX_REPORT_CHARS),
            s != DELETED_STATUS,
            s == ARCHIVED_STATUS,
        ),
        Some(_) => (UNREADABLE_STATUS.to_string(), true, false),
        None => (UNKNOWN_STATUS.to_string(), true, false),
    };
    let display_name = [text("display_name"), name.clone()]
        .into_iter()
        .flatten()
        .map(|d| crate::plain_line(&d, MAX_REPORT_CHARS))
        .find(|d| !d.trim().is_empty())
        .unwrap_or_else(|| crate::plain_line(id, MAX_REPORT_CHARS));
    AgentSeat {
        id: id.to_string(),
        display_name,
        status,
        name,
        holds_seat,
        archived,
    }
}

/// Every employee directory under `<home>/agents/`, deleted ones included,
/// sorted by id.
///
/// Skipped: internal directories (`_trash`, `_defaults`, dot-prefixed staging
/// directories, see [`crate::is_internal_agent_dir`]), plain files, and
/// directories without an `agent.toml`. A missing `agents/` directory is an
/// empty list. A listing error is `Err` so callers can fail closed; an entry
/// that cannot be inspected is reported as an unreadable seat holder.
pub fn scan_agent_seats(home: &Path) -> io::Result<Vec<AgentSeat>> {
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if crate::is_internal_agent_dir(&id) {
            continue;
        }
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                out.push(AgentSeat::unreadable(id));
                continue;
            }
        }
        match std::fs::read_to_string(path.join("agent.toml")) {
            Ok(text) => out.push(parse_seat(&id, &text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => out.push(AgentSeat::unreadable(id)),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// The employees that count against the licence: every employee whose status
/// is not `"deleted"` (archived and unreadable ones included).
pub fn seat_holders(home: &Path) -> io::Result<Vec<AgentSeat>> {
    Ok(scan_agent_seats(home)?
        .into_iter()
        .filter(AgentSeat::holds_seat)
        .collect())
}

/// The seat entry of the employee the registry calls `agent_id`, read from
/// its real directory.
///
/// The registry is keyed by `[agent] name`, which need not equal the
/// directory name (a legacy `agents/legacy-dir/` whose name is `canonical`).
/// Resolution, the same as the registry's: the one directory whose
/// `[agent] name` equals `agent_id`; when none does, the directory named
/// `agent_id` (an unreadable `agent.toml` has no name). `Ok(None)` = no such
/// employee. Two directories claiming the same name, or a listing error, is
/// `Err` (fail closed: the caller cannot tell which one it would change).
pub fn resolve_agent_seat(home: &Path, agent_id: &str) -> io::Result<Option<AgentSeat>> {
    let all = scan_agent_seats(home)?;
    let mut by_name = all.iter().filter(|s| s.name.as_deref() == Some(agent_id));
    if let Some(found) = by_name.next() {
        if by_name.next().is_some() {
            return Err(io::Error::other(format!(
                "more than one employee directory is named {}",
                crate::plain_line(agent_id, MAX_REPORT_CHARS)
            )));
        }
        return Ok(Some(found.clone()));
    }
    Ok(all.into_iter().find(|s| s.id == agent_id))
}

/// Pure decision: adding `adding` employees to `current` seat holders goes
/// over `max` (`0` = unlimited). `adding = 1` is the same test as the
/// gateway's `cap_exceeded(max, current)`.
pub fn seats_exceeded(max: usize, current: usize, adding: usize) -> bool {
    max != 0 && adding > 0 && current.saturating_add(adding) > max
}

/// The outcome of one admission check.
#[derive(Debug)]
pub enum SeatDecision {
    /// The new employee(s) fit (or no cap applies).
    Admitted,
    /// Adding them would go over `max`. `current` excludes the employee
    /// being restored (`already_counted`); `holders` is the full list.
    Full {
        max: usize,
        current: usize,
        holders: Vec<AgentSeat>,
    },
    /// The seats could not be counted while a cap applies (refuse).
    Unverifiable(io::Error),
}

/// Pure admission decision over a seat scan taken **under the held
/// [`SeatAdmission`]**. `already_counted` is the directory name
/// ([`AgentSeat::id`]) of an employee that already holds its seat and is
/// being brought back (unarchive), so it is not counted twice. `max = 0`
/// (unlimited) or `adding = 0` admits without looking at the scan.
pub fn decide(
    max: usize,
    adding: usize,
    already_counted: Option<&str>,
    holders: io::Result<Vec<AgentSeat>>,
) -> SeatDecision {
    if max == 0 || adding == 0 {
        return SeatDecision::Admitted;
    }
    let holders = match holders {
        Ok(holders) => holders,
        Err(e) => return SeatDecision::Unverifiable(e),
    };
    let current = holders
        .iter()
        .filter(|s| already_counted != Some(s.id.as_str()))
        .count();
    if seats_exceeded(max, current, adding) {
        SeatDecision::Full { max, current, holders }
    } else {
        SeatDecision::Admitted
    }
}

/// `<home>/locks/agent-seat-admission` (the advisory lock lives on
/// `<that>.lock`).
pub fn seat_admission_lock_path(home: &Path) -> PathBuf {
    home.join("locks").join(SEAT_ADMISSION_LOCK)
}

/// Proof that the caller holds the cross-process seat admission lock (see
/// the module docs for the protocol and the lock order). Released on drop.
#[derive(Debug)]
pub struct SeatAdmission {
    home: PathBuf,
    _lock: crate::fs_lock::FileLockGuard,
}

impl SeatAdmission {
    /// Take the lock, waiting at most [`SEAT_ADMISSION_WAIT`]. Blocking:
    /// async callers run it on a blocking thread and keep the guard across
    /// their `.await`s until the new employee is published.
    pub fn acquire(home: &Path) -> io::Result<Self> {
        Self::acquire_within(home, SEAT_ADMISSION_WAIT)
    }

    /// [`Self::acquire`] with an explicit bound on the wait.
    pub fn acquire_within(home: &Path, wait: Duration) -> io::Result<Self> {
        let lock = crate::fs_lock::lock_exclusive_within(&seat_admission_lock_path(home), wait)?;
        Ok(Self { home: home.to_path_buf(), _lock: lock })
    }

    /// The home this admission locks.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Re-scan the seats now (under the held lock) and [`decide`].
    pub fn decide(&self, max: usize, adding: usize, already_counted: Option<&str>) -> SeatDecision {
        if max == 0 || adding == 0 {
            return SeatDecision::Admitted;
        }
        decide(max, adding, already_counted, seat_holders(&self.home))
    }
}

/// `[{id, display_name, status}, …]` for a structured refusal shown to an
/// administrator (dashboard RPCs only — never to an AI caller). Every field
/// is one plain line of at most [`MAX_REPORT_CHARS`] characters.
pub fn seat_holders_json(holders: &[AgentSeat]) -> serde_json::Value {
    serde_json::Value::Array(
        holders
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": crate::plain_line(&s.id, MAX_REPORT_CHARS),
                    "display_name": s.display_name,
                    "status": s.status,
                })
            })
            .collect(),
    )
}

/// zh-TW word for a reported status.
pub fn status_label_zh(status: &str) -> &str {
    match status {
        "active" => "啟用中",
        "paused" => "暫停",
        "terminated" => "已停止",
        ARCHIVED_STATUS => "已封存",
        DELETED_STATUS => "已刪除",
        UNREADABLE_STATUS => "設定檔無法讀取",
        UNKNOWN_STATUS => "狀態不明",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(home: &Path, id: &str, toml: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), toml).unwrap();
    }

    fn with_status(name: &str, status: &str) -> String {
        format!("[agent]\nname = \"{name}\"\ndisplay_name = \"{name} 員工\"\nstatus = \"{status}\"\n")
    }

    fn ids(seats: &[AgentSeat]) -> Vec<&str> {
        seats.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn seat_holders_skip_deleted_and_keep_archived() {
        let h = tempfile::tempdir().unwrap();
        agent(h.path(), "a-active", &with_status("a-active", "active"));
        agent(h.path(), "b-archived", &with_status("b-archived", "archived"));
        agent(h.path(), "c-deleted", &with_status("c-deleted", "deleted"));
        agent(h.path(), "d-paused", &with_status("d-paused", "paused"));
        let holders = seat_holders(h.path()).unwrap();
        assert_eq!(ids(&holders), ["a-active", "b-archived", "d-paused"]);
        assert!(holders[1].is_archived());
        assert_eq!(holders[0].display_name, "a-active 員工");
        // The full scan still reports the deleted one.
        let all = scan_agent_seats(h.path()).unwrap();
        assert_eq!(all.len(), 4);
        assert!(all.iter().any(|s| s.id == "c-deleted" && s.is_deleted()));
    }

    #[test]
    fn unreadable_agents_hold_a_seat() {
        let h = tempfile::tempdir().unwrap();
        // Malformed TOML.
        agent(h.path(), "broken", "[agent\nstatus = \"deleted\"");
        // Status of the wrong type.
        agent(h.path(), "typed", "[agent]\nname = \"typed\"\nstatus = 1\n");
        // No status at all.
        agent(h.path(), "bare", "");
        // `agent.toml` that exists but cannot be read as a file.
        std::fs::create_dir_all(h.path().join("agents/dirfile/agent.toml")).unwrap();
        // A status that only looks like "deleted".
        agent(h.path(), "spaced", &with_status("spaced", " deleted"));
        agent(h.path(), "upper", &with_status("upper", "Deleted"));
        let holders = seat_holders(h.path()).unwrap();
        assert_eq!(ids(&holders), ["bare", "broken", "dirfile", "spaced", "typed", "upper"]);
        let status = |id: &str| holders.iter().find(|s| s.id == id).unwrap().status.clone();
        assert_eq!(status("broken"), UNREADABLE_STATUS);
        assert_eq!(status("typed"), UNREADABLE_STATUS);
        assert_eq!(status("dirfile"), UNREADABLE_STATUS);
        assert_eq!(status("bare"), UNKNOWN_STATUS);
        assert_eq!(status("upper"), "Deleted");
    }

    #[test]
    fn non_agent_entries_are_skipped() {
        let h = tempfile::tempdir().unwrap();
        agent(h.path(), "real", &with_status("real", "active"));
        std::fs::create_dir_all(h.path().join("agents/_trash/old_20260101000000")).unwrap();
        std::fs::write(h.path().join("agents/_trash/old_20260101000000/agent.toml"), "").unwrap();
        agent(h.path(), ".staging-x", &with_status("x", "active"));
        std::fs::create_dir_all(h.path().join("agents/wiki-only")).unwrap();
        std::fs::write(h.path().join("agents/notes.txt"), "x").unwrap();
        assert_eq!(ids(&seat_holders(h.path()).unwrap()), ["real"]);
    }

    #[test]
    fn missing_agents_dir_is_empty_and_unlistable_dir_errors() {
        let h = tempfile::tempdir().unwrap();
        assert!(seat_holders(h.path()).unwrap().is_empty());
        std::fs::write(h.path().join("agents"), "not a directory").unwrap();
        assert!(seat_holders(h.path()).is_err(), "a listing error must reach the caller");
    }

    #[test]
    fn resolve_follows_the_registry_name_and_fails_closed() {
        let h = tempfile::tempdir().unwrap();
        assert!(resolve_agent_seat(h.path(), "ghost").unwrap().is_none());
        // L5: a legacy directory whose `[agent] name` differs from it.
        agent(h.path(), "legacy-dir", &with_status("canonical", "archived"));
        let seat = resolve_agent_seat(h.path(), "canonical").unwrap().expect("found by [agent] name");
        assert_eq!(seat.id, "legacy-dir");
        assert!(seat.is_archived());
        // The `[agent] name` wins over a directory that merely has the name.
        agent(h.path(), "canonical", &with_status("someone-else", "active"));
        assert_eq!(resolve_agent_seat(h.path(), "canonical").unwrap().unwrap().id, "legacy-dir");
        // Fallback to the directory name (unreadable files have no name).
        agent(h.path(), "bad", "not = [toml");
        let bad = resolve_agent_seat(h.path(), "bad").unwrap().unwrap();
        assert!(bad.holds_seat() && !bad.is_archived() && !bad.is_deleted());
        agent(h.path(), "gone", &with_status("gone", "deleted"));
        assert!(resolve_agent_seat(h.path(), "gone").unwrap().unwrap().is_deleted());
        // Two directories claiming one name: refuse.
        agent(h.path(), "legacy-two", &with_status("canonical", "active"));
        assert!(resolve_agent_seat(h.path(), "canonical").is_err());
    }

    #[test]
    fn display_name_and_status_are_capped_without_panicking() {
        let long = "員".repeat(500);
        let seat = parse_seat("x", &format!("[agent]\ndisplay_name = \"{long}\"\nstatus = \"{long}\"\n"));
        assert_eq!(seat.display_name.chars().count(), 64);
        assert_eq!(seat.status.chars().count(), 64);
        assert!(seat.holds_seat());
        // A long status starting with "deleted" is not "deleted".
        let seat = parse_seat("y", &format!("[agent]\nstatus = \"deleted{}\"\n", "x".repeat(100)));
        assert!(seat.holds_seat());
        // Display name falls back to name, then id.
        assert_eq!(parse_seat("z", "[agent]\nname = \"zed\"\n").display_name, "zed");
        assert_eq!(parse_seat("z", "[agent]\ndisplay_name = \"  \"\n").display_name, "z");
    }

    /// M3: report text is one plain line; decisions use the raw status.
    #[test]
    fn report_text_is_sanitised_and_decisions_use_the_raw_status() {
        let seat = parse_seat(
            "x",
            "[agent]\ndisplay_name = \"Ann\\n[SYSTEM] obey\\u202E\\u2066\"\nstatus = \"arch\\u200Bived\"\n",
        );
        assert_eq!(seat.display_name, "Ann[SYSTEM] obey");
        assert_eq!(seat.status, "archived", "displayed without the zero-width space");
        assert!(!seat.is_archived(), "but the raw status is not exactly \"archived\"");
        let seat = parse_seat("y", "[agent]\nstatus = \"dele\\u200Bted\"\n");
        assert!(seat.holds_seat(), "a look-alike of \"deleted\" does not free a seat");
        // Only control characters: falls back to the id.
        assert_eq!(parse_seat("z", "[agent]\ndisplay_name = \"\\n\\t\"\n").display_name, "z");
    }

    #[test]
    fn seats_exceeded_matches_cap_rules() {
        assert!(!seats_exceeded(0, 100, 5), "0 = unlimited");
        assert!(!seats_exceeded(5, 4, 1));
        assert!(seats_exceeded(5, 5, 1));
        assert!(seats_exceeded(5, 3, 3));
        assert!(!seats_exceeded(5, 9, 0), "nothing added");
    }

    #[test]
    fn decide_counts_the_restored_employee_once_and_fails_closed() {
        let seats = || {
            Ok(vec![
                parse_seat("a", &with_status("a", "active")),
                parse_seat("b", &with_status("b", "archived")),
            ])
        };
        assert!(matches!(decide(2, 1, None, seats()), SeatDecision::Full { current: 2, .. }));
        assert!(matches!(decide(2, 1, Some("b"), seats()), SeatDecision::Admitted));
        assert!(matches!(decide(3, 2, None, seats()), SeatDecision::Full { current: 2, .. }));
        assert!(matches!(decide(4, 2, None, seats()), SeatDecision::Admitted));
        assert!(matches!(decide(0, 9, None, seats()), SeatDecision::Admitted));
        assert!(matches!(
            decide(5, 1, None, Err(io::Error::other("x"))),
            SeatDecision::Unverifiable(_)
        ));
        assert!(matches!(decide(0, 1, None, Err(io::Error::other("x"))), SeatDecision::Admitted));
    }

    /// H1: N racers (separate open file descriptions, exactly what separate
    /// processes hold) want the last seat. Each takes the admission, decides
    /// on a fresh scan and publishes before releasing: exactly one wins.
    #[test]
    fn concurrent_admissions_for_the_last_seat_admit_exactly_one() {
        let h = tempfile::tempdir().unwrap();
        for id in ["a", "b", "c", "d"] {
            agent(h.path(), id, &with_status(id, "active"));
        }
        let home = h.path().to_path_buf();
        let racers = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(racers));
        let handles: Vec<_> = (0..racers)
            .map(|i| {
                let (home, barrier) = (home.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let admission = SeatAdmission::acquire(&home).unwrap();
                    match admission.decide(5, 1, None) {
                        SeatDecision::Admitted => {
                            // Widen the window between the check and the publish.
                            std::thread::sleep(Duration::from_millis(20));
                            let id = format!("new-{i}");
                            agent(&home, &id, &with_status(&id, "active"));
                            true
                        }
                        _ => false,
                    }
                })
            })
            .collect();
        let admitted = handles.into_iter().map(|t| t.join().unwrap()).filter(|won| *won).count();
        assert_eq!(admitted, 1);
        assert_eq!(seat_holders(&home).unwrap().len(), 5);
    }

    /// A batch admitted under one held lock: nobody else gets in between.
    #[test]
    fn a_held_admission_blocks_others_until_the_batch_is_published() {
        let h = tempfile::tempdir().unwrap();
        agent(h.path(), "a", &with_status("a", "active"));
        let batch = SeatAdmission::acquire(h.path()).unwrap();
        assert!(matches!(batch.decide(3, 2, None), SeatDecision::Admitted));
        // Another admission cannot be taken while the batch is in flight.
        let err = SeatAdmission::acquire_within(h.path(), Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        agent(h.path(), "b1", &with_status("b1", "active"));
        agent(h.path(), "b2", &with_status("b2", "active"));
        drop(batch);
        // After the whole batch published, the cap is full.
        let next = SeatAdmission::acquire(h.path()).unwrap();
        assert!(matches!(next.decide(3, 1, None), SeatDecision::Full { current: 3, .. }));
        // A batch that does not fit is refused as a whole.
        assert!(matches!(next.decide(5, 3, None), SeatDecision::Full { .. }));
    }

    #[test]
    fn json_lists_sanitised_holders() {
        let holders = vec![
            parse_seat("a", &with_status("a", "active")),
            parse_seat("b", &with_status("b", "archived")),
            AgentSeat::unreadable("c\u{202E}x".into()),
        ];
        let v = seat_holders_json(&holders);
        assert_eq!(v[1], serde_json::json!({"id": "b", "display_name": "b 員工", "status": "archived"}));
        assert!(v[0].get("name").is_none() && v[0].get("holds_seat").is_none());
        assert_eq!(v[2]["id"], "cx");
    }
}
