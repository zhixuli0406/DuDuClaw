//! The journal of an import replace in progress, and putting things back
//! when a replace stopped half way.
//!
//! `duduclaw pack install --replace <slug>` moves the earlier import aside
//! (employees into the trash, the rest into `experts/_replaced/<backup>/`)
//! before the new version writes. Before the first move it writes
//! `experts/_replaced/<backup>.journal.json`: every planned move, the org
//! entries of the employees it moves, and — as the new version goes on —
//! every path the new version starts creating. The journal is removed when
//! the replace succeeds or its rollback is done.
//!
//! A journal still there afterwards means the installer was stopped (killed
//! at the dashboard's timeout, the machine went down). [`recover_interrupted`]
//! runs at gateway start and before every import: for each journal whose
//! process is gone it moves what the new version wrote into
//! `<backup>/failed-new/`, moves the earlier import back, restores the org
//! entries, withdraws approvals the half-installed version filed, writes an
//! audit event and a warning, and removes the journal.
//!
//! Every path in a journal is relative to the home and made of plain
//! components only; a journal with anything else is left alone (and
//! reported), never followed.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use duduclaw_core::org_store::{self, OrgEntry};

use crate::expert_admin::experts_dir;

/// Folder under `experts/` holding replace backups and their journals.
pub const REPLACED_DIR: &str = "_replaced";

const JOURNAL_SUFFIX: &str = ".journal.json";

/// Paths the new version writes are appended to this sidecar, one per
/// line (a pack can carry thousands of pages; rewriting the journal for
/// each would be quadratic).
const WRITTEN_SUFFIX: &str = ".journal.paths";

/// A journal older than this counts as abandoned even when its process id
/// is in use again (an import is stopped after minutes, never hours).
const STALE_AFTER_SECS: i64 = 2 * 60 * 60;

/// An org entry as the journal keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalOrgEntry {
    pub reports_to: String,
    pub department: String,
}

/// One replace in progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplaceJournal {
    pub slug: String,
    /// The installer's process id.
    pub pid: u32,
    pub started_at: String,
    /// `experts/_replaced/<name>` (home-relative).
    pub backup: String,
    /// Planned moves `(from, to)`, home-relative, in order.
    pub moves: Vec<(String, String)>,
    /// Org entries of the moved employees before the replace (`None`:
    /// there was none).
    pub org_before: Vec<(String, Option<JournalOrgEntry>)>,
    /// Employee ids the new version started creating.
    pub written_agents: Vec<String>,
    /// Paths (home-relative) the new version started creating.
    pub written_paths: Vec<String>,
}

/// What undoing a replace did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    /// Earlier-import paths moved back.
    pub restored: usize,
    /// New-version paths moved into `failed-new/`.
    pub moved_aside: usize,
    /// Steps that failed (printed / logged; finish by hand).
    pub failures: Vec<String>,
    /// Where the new version's record folder went (its approvals).
    pub failed_record: Option<PathBuf>,
}

impl ReplaceJournal {
    pub fn new(home: &Path, slug: &str, backup: &Path) -> Self {
        ReplaceJournal {
            slug: slug.to_string(),
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339(),
            backup: rel(home, backup),
            ..Default::default()
        }
    }

    pub fn plan_move(&mut self, home: &Path, from: &Path, to: &Path) {
        self.moves.push((rel(home, from), rel(home, to)));
    }

    pub fn keep_org(&mut self, id: &str, entry: Option<&OrgEntry>) {
        let entry = entry.map(|e| JournalOrgEntry {
            reports_to: e.reports_to.clone(),
            department: e.department.clone(),
        });
        self.org_before.push((id.to_string(), entry));
    }

    pub fn note_agent(&mut self, home: &Path, id: &str) {
        if !self.written_agents.iter().any(|a| a == id) {
            self.written_agents.push(id.to_string());
        }
        self.note_path(home, &home.join("agents").join(id));
    }

    /// Record a path in the journal itself (written with the next
    /// [`Self::save`]).
    pub fn note_path(&mut self, home: &Path, path: &Path) {
        let r = rel(home, path);
        if !self.written_paths.contains(&r) {
            self.written_paths.push(r);
        }
    }

    /// Record a path the new version is about to create: kept in memory
    /// and appended to the sidecar on disk right away (no full rewrite).
    pub fn append_written(&mut self, home: &Path, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let r = rel(home, path);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.sidecar(home))?;
        f.write_all(format!("{r}\n").as_bytes())?;
        self.written_paths.push(r);
        Ok(())
    }

    fn name(&self) -> String {
        Path::new(&self.backup)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `experts/_replaced/<backup>.journal.json`.
    pub fn path(&self, home: &Path) -> PathBuf {
        experts_dir(home).join(REPLACED_DIR).join(format!("{}{JOURNAL_SUFFIX}", self.name()))
    }

    fn sidecar(&self, home: &Path) -> PathBuf {
        experts_dir(home).join(REPLACED_DIR).join(format!("{}{WRITTEN_SUFFIX}", self.name()))
    }

    /// Write (or rewrite) the journal atomically.
    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        let path = self.path(home);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &path)
    }

    /// The replace finished (installed, or rolled back): drop the journal.
    pub fn remove(&self, home: &Path) {
        let _ = std::fs::remove_file(self.path(home));
        let _ = std::fs::remove_file(self.sidecar(home));
    }

    fn checked(&self) -> Result<(), String> {
        let paths = std::iter::once(&self.backup)
            .chain(self.moves.iter().flat_map(|(a, b)| [a, b]))
            .chain(self.written_paths.iter());
        for p in paths {
            if p.is_empty() || !Path::new(p).components().all(|c| matches!(c, Component::Normal(_))) {
                return Err(format!("journal path {} is not a plain home-relative path", p.escape_debug()));
            }
        }
        if !self.backup.starts_with(&format!("experts/{REPLACED_DIR}/")) {
            return Err("journal backup is outside experts/_replaced".into());
        }
        for id in self.org_before.iter().map(|(id, _)| id).chain(self.written_agents.iter()) {
            if !duduclaw_core::is_valid_agent_id(id) {
                return Err(format!("journal agent id {} is not valid", id.escape_debug()));
            }
        }
        Ok(())
    }
}

fn rel(home: &Path, p: &Path) -> String {
    let r = p.strip_prefix(home).unwrap_or(p);
    r.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

fn move_into(from: &Path, to: &Path) -> std::io::Result<()> {
    if exists(to) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "目的地已存在"));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)
}

/// Undo the replace described by `j`: what the new version wrote goes into
/// `<backup>/failed-new/` (same layout as the home), every move that
/// happened is reversed (newest first), the org entries are put back.
/// Works from any point of the replace — a move that never happened is
/// skipped. Best effort: failed steps are listed.
pub fn undo(home: &Path, j: &ReplaceJournal) -> UndoOutcome {
    let mut out = UndoOutcome::default();
    if let Err(e) = j.checked() {
        out.failures.push(e);
        return out;
    }
    let backup = home.join(&j.backup);
    let aside = |p: &Path, out: &mut UndoOutcome| -> Option<PathBuf> {
        let base = backup.join("failed-new").join(rel(home, p));
        let mut target = base.clone();
        let mut n = 2;
        while exists(&target) {
            let name = base.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            target.set_file_name(format!("{name}-{n}"));
            n += 1;
        }
        match move_into(p, &target) {
            Ok(()) => {
                out.moved_aside += 1;
                prune_empty_parents(home, p);
                Some(target)
            }
            Err(e) => {
                out.failures.push(format!("{} 無法移開：{e}", p.display()));
                None
            }
        }
    };
    let record_dir = experts_dir(home).join(&j.slug);
    let originals: BTreeSet<&str> = j.moves.iter().map(|(from, _)| from.as_str()).collect();
    for p in j.written_paths.iter().rev() {
        let abs = home.join(p);
        if !originals.contains(p.as_str()) && exists(&abs) {
            let went = aside(&abs, &mut out);
            if abs == record_dir {
                out.failed_record = went;
            }
        }
    }
    for (from, to) in j.moves.iter().rev() {
        let (from, to) = (home.join(from), home.join(to));
        if !exists(&to) {
            continue; // never moved (or already back)
        }
        if exists(&from) {
            let went = aside(&from, &mut out);
            if went.is_none() {
                continue;
            }
            if from == record_dir {
                out.failed_record = went;
            }
        }
        match std::fs::rename(&to, &from) {
            Ok(()) => out.restored += 1,
            Err(e) => out.failures.push(format!("{} → {}：{e}（請手動搬回）", to.display(), from.display())),
        }
    }
    restore_org(home, j, &mut out);
    // Only removes the backup folder when nothing is left in it.
    for sub in ["skills", "wiki", ""] {
        let _ = std::fs::remove_dir(backup.join(sub));
    }
    out
}

/// Folders the new version created only to hold what was just moved
/// aside (`shared/wiki/<a>/<b>/` for a page) are removed when empty; the
/// top two levels (`shared/wiki`, `agents`, …) always stay.
fn prune_empty_parents(home: &Path, moved: &Path) {
    let mut dir = moved.parent();
    while let Some(d) = dir {
        let depth = d.strip_prefix(home).map(|r| r.components().count()).unwrap_or(0);
        if depth <= 2 || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn restore_org(home: &Path, j: &ReplaceJournal, out: &mut UndoOutcome) {
    for (id, before) in &j.org_before {
        let res = match before {
            Some(e) => org_store::upsert(home, id, OrgEntry::new(&e.reports_to, &e.department)).map(|_| ()),
            None => org_store::remove(home, id).map(|_| ()),
        };
        if let Err(e) = res {
            out.failures.push(format!("組織資料還原失敗（{id}）：{e}"));
        }
    }
    for id in &j.written_agents {
        if j.org_before.iter().any(|(old, _)| old == id) {
            continue;
        }
        if let Err(e) = org_store::remove(home, id) {
            out.failures.push(format!("組織資料還原失敗（{id}）：{e}"));
        }
    }
}

/// Withdraw the approvals still pending for an import that is out of use
/// (its record folder is `record_dir`): the skills and hooks requests it
/// filed. A request already decided is left as it is. Returns how many
/// were withdrawn.
pub async fn withdraw_pending(home: &Path, record_dir: &Path, reason: &str) -> usize {
    let ids: Vec<String> = ["skills-state.json", "hooks-state.json"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(record_dir.join(f)).ok())
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter_map(|v| v.get("approval_id").and_then(|i| i.as_str()).map(str::to_string))
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return 0;
    }
    let broker = match crate::approval::ApprovalBroker::open(home) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("could not open approvals to withdraw an out-of-use import's requests: {e}");
            return 0;
        }
    };
    let mut n = 0;
    for id in ids {
        match broker.withdraw(&crate::approval::ApprovalId::from(id.clone()), reason).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("withdrawing approval {id} failed: {e}"),
        }
    }
    n
}

/// Which journals [`recover_interrupted`] may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverScope {
    /// Only journals whose installer is gone (gateway start, an import
    /// about to begin).
    Abandoned,
    /// Also this process's own (the installer itself was told to stop).
    IncludingOwn,
}

/// One replace put back by [`recover_interrupted`].
#[derive(Debug, Clone)]
pub struct Recovered {
    pub slug: String,
    pub outcome: UndoOutcome,
    pub withdrawn_approvals: usize,
}

fn abandoned(j: &ReplaceJournal, scope: RecoverScope) -> bool {
    if j.pid == std::process::id() {
        return scope == RecoverScope::IncludingOwn;
    }
    let old = chrono::DateTime::parse_from_rfc3339(&j.started_at)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() > STALE_AFTER_SECS)
        .unwrap_or(true);
    old || !duduclaw_core::platform::process_alive(j.pid)
}

/// The journals currently in the home (unreadable ones are skipped with a
/// warning).
pub fn list(home: &Path) -> Vec<ReplaceJournal> {
    let dir = experts_dir(home).join(REPLACED_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(JOURNAL_SUFFIX) {
            continue;
        }
        match std::fs::read_to_string(entry.path()).map(|raw| serde_json::from_str::<ReplaceJournal>(&raw)) {
            Ok(Ok(mut j)) => {
                if let Ok(extra) = std::fs::read_to_string(j.sidecar(home)) {
                    j.written_paths.extend(extra.lines().filter(|l| !l.is_empty()).map(str::to_string));
                }
                out.push(j)
            }
            Ok(Err(e)) => tracing::warn!("replace journal {} unreadable: {e}", entry.path().display()),
            Err(e) => tracing::warn!("replace journal {} unreadable: {e}", entry.path().display()),
        }
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    out
}

/// Put back every replace that stopped half way (see the module docs).
/// Each one is logged as a warning and recorded in the security audit log
/// (`expert_replace_recovered`).
pub async fn recover_interrupted(home: &Path, scope: RecoverScope) -> Vec<Recovered> {
    let mut done = Vec::new();
    for j in list(home) {
        if !abandoned(&j, scope) {
            continue;
        }
        let outcome = undo(home, &j);
        let withdrawn = match &outcome.failed_record {
            Some(dir) => withdraw_pending(home, dir, "import_interrupted").await,
            None => 0,
        };
        tracing::warn!(
            slug = %j.slug,
            restored = outcome.restored,
            moved_aside = outcome.moved_aside,
            failures = outcome.failures.len(),
            "an import replace stopped half way; the earlier version was put back"
        );
        duduclaw_security::audit::append_audit_event(
            home,
            &duduclaw_security::audit::AuditEvent::new(
                "expert_replace_recovered",
                "system",
                duduclaw_security::audit::Severity::Warning,
                serde_json::json!({
                    "slug": j.slug,
                    "pid": j.pid,
                    "started_at": j.started_at,
                    "backup": j.backup,
                    "restored": outcome.restored,
                    "moved_aside": outcome.moved_aside,
                    "withdrawn_approvals": withdrawn,
                    "failures": outcome.failures,
                }),
            ),
        );
        // A journal whose undo could not even start (unsafe paths) stays
        // for a person to look at.
        if outcome.restored > 0 || outcome.moved_aside > 0 || outcome.failures.is_empty() {
            j.remove(home);
        }
        done.push(Recovered { slug: j.slug.clone(), outcome, withdrawn_approvals: withdrawn });
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            c.args(["/C", "exit"]);
        }
        let mut child = c.spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// A home where `team` was replaced half way: the earlier employee and
    /// record are in the backup, the new version wrote `lead` and `helper`.
    fn half_replaced(home: &Path, pid: u32) -> ReplaceJournal {
        let backup = experts_dir(home).join(REPLACED_DIR).join("team_20261023000000");
        let mut j = ReplaceJournal::new(home, "team", &backup);
        j.pid = pid;
        let trash = home.join("agents/_trash/lead_20261023000000");
        j.plan_move(home, &home.join("agents/lead"), &trash);
        j.plan_move(home, &experts_dir(home).join("team"), &backup.join("record"));
        j.keep_org("lead", Some(&OrgEntry::new("", "銷售")));
        j.note_agent(home, "lead");
        j.note_path(home, &experts_dir(home).join("team"));
        j.save(home).unwrap();
        // Written later, through the sidecar.
        j.written_agents.push("helper".into());
        j.save(home).unwrap();
        j.append_written(home, &home.join("agents/helper")).unwrap();
        // The moves happened; the new version then wrote.
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("SOUL.md"), "old").unwrap();
        std::fs::create_dir_all(backup.join("record")).unwrap();
        std::fs::write(backup.join("record/install.json"), "{}").unwrap();
        for id in ["lead", "helper"] {
            std::fs::create_dir_all(home.join("agents").join(id)).unwrap();
            std::fs::write(home.join("agents").join(id).join("SOUL.md"), "new").unwrap();
            org_store::upsert(home, id, OrgEntry::new("", "客服")).unwrap();
        }
        std::fs::create_dir_all(experts_dir(home).join("team")).unwrap();
        let page = home.join("shared/wiki/new/deep/p.md");
        std::fs::create_dir_all(page.parent().unwrap()).unwrap();
        std::fs::write(&page, "x").unwrap();
        j.append_written(home, &page).unwrap();
        j
    }

    #[tokio::test]
    async fn an_abandoned_replace_is_put_back_and_audited() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let j = half_replaced(h, dead_pid());
        let done = recover_interrupted(h, RecoverScope::Abandoned).await;
        assert_eq!(done.len(), 1);
        assert!(done[0].outcome.failures.is_empty(), "{:?}", done[0].outcome);
        assert_eq!(std::fs::read_to_string(h.join("agents/lead/SOUL.md")).unwrap(), "old");
        assert!(!h.join("agents/helper").exists());
        assert!(h.join("experts/team/install.json").is_file());
        assert!(!h.join("shared/wiki/new/deep").exists(), "empty folders of moved pages pruned");
        assert!(h.join("shared/wiki").is_dir());
        let failed = h.join("experts/_replaced/team_20261023000000/failed-new");
        assert!(failed.join("agents/helper/SOUL.md").is_file());
        assert!(failed.join("agents/lead/SOUL.md").is_file());
        let org = org_store::load(h);
        assert_eq!(org.get("lead").map(|e| e.department.as_str()), Some("銷售"));
        assert!(org.get("helper").is_none());
        assert!(!j.path(h).exists(), "journal removed");
        let audit = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("expert_replace_recovered"), "{audit}");
    }

    #[tokio::test]
    async fn a_replace_still_running_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let j = half_replaced(h, std::process::id());
        assert!(recover_interrupted(h, RecoverScope::Abandoned).await.is_empty());
        assert!(j.path(h).exists());
        assert_eq!(std::fs::read_to_string(h.join("agents/lead/SOUL.md")).unwrap(), "new");
        // The installer itself, told to stop, puts it back.
        assert_eq!(recover_interrupted(h, RecoverScope::IncludingOwn).await.len(), 1);
        assert_eq!(std::fs::read_to_string(h.join("agents/lead/SOUL.md")).unwrap(), "old");
    }

    #[tokio::test]
    async fn a_journal_with_unsafe_paths_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), "x").unwrap();
        let backup = experts_dir(h).join(REPLACED_DIR).join("evil_1");
        let mut j = ReplaceJournal::new(h, "evil", &backup);
        j.pid = dead_pid();
        j.written_paths.push("../../etc".into());
        j.save(h).unwrap();
        let done = recover_interrupted(h, RecoverScope::Abandoned).await;
        assert_eq!(done.len(), 1);
        assert!(!done[0].outcome.failures.is_empty());
        assert!(j.path(h).exists(), "kept for a person to look at");
    }
}
