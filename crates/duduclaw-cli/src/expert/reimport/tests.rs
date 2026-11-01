//! Re-import (replace / copy), the agent cap and "nothing imported" through
//! the real pipeline, in isolated temp homes.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ReimportMode;
use crate::expert::import_error::failure_of;
use crate::expert::install::PipelineOpts;
use crate::expert::agent_config::preview::confirmed_install as cmd_install_with;

/// An instructions-format source: `<dir>/<name>/AGENTS.md` (id and import
/// name are both `<name>`).
fn source(dir: &Path, name: &str, rules: &str) -> PathBuf {
    let d = dir.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("AGENTS.md"), format!("# 工作規則\n\n{rules}\n")).unwrap();
    d
}

fn opts(dry_run: bool, mode: ReimportMode, report: &Path) -> PipelineOpts {
    PipelineOpts {
        dry_run,
        reimport: mode,
        report_json: Some(report.to_path_buf()),
        ..Default::default()
    }
}

/// Run one install; returns the failure code (if any) and the report.
async fn run(home: &Path, src: &Path, dry_run: bool, mode: ReimportMode) -> (Option<&'static str>, Value) {
    let rpt = tempfile::tempdir().unwrap();
    let path = rpt.path().join("r.json");
    let res = cmd_install_with(home, src.to_str().unwrap(), opts(dry_run, mode, &path)).await;
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let code = res.as_ref().err().map(|e| failure_of(e).map(|f| f.code).unwrap_or("unclassified"));
    if let Some(c) = code {
        assert_eq!(report["error"]["code"], c, "report carries the same code");
    }
    (code, report)
}

fn replace(slug: &str, accept_edits: bool) -> ReimportMode {
    ReimportMode::Replace { slug: slug.into(), accept_edits }
}

fn trash_entries(home: &Path, id: &str) -> Vec<String> {
    std::fs::read_dir(home.join("agents/_trash"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| duduclaw_core::agent_trash::trash_entry_agent_id(n) == Some(id))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_second_import_without_a_choice_is_refused_with_its_options() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "先讀資料。");
    let (code, _) = run(home.path(), &pack, false, ReimportMode::Fresh).await;
    assert_eq!(code, None);

    let (code, report) = run(home.path(), &pack, true, ReimportMode::Fresh).await;
    assert_eq!(code, Some("import.already_imported"));
    let p = &report["error"]["params"];
    assert_eq!(p["slug"], "my-advisor");
    assert_eq!(p["agents"][0], "my-advisor");
    assert_eq!(p["edited"], false);
    // No CLI hint in the machine-readable part.
    assert!(!p.to_string().contains("duduclaw"));
}

#[tokio::test]
async fn import_as_copy_keeps_the_first_and_renames_the_second() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "先讀資料。");
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Fresh).await.0, None);
    let (code, plan) = run(home.path(), &pack, true, ReimportMode::Copy).await;
    assert_eq!(code, None, "{plan}");
    let (code, _) = run(home.path(), &pack, false, ReimportMode::Copy).await;
    assert_eq!(code, None);
    let first = crate::expert::read_record(home.path(), "my-advisor").unwrap();
    let copy = crate::expert::read_record(home.path(), "my-advisor-2").unwrap();
    assert_eq!(first.agents, vec!["my-advisor".to_string()]);
    assert_eq!(copy.agents.len(), 1);
    assert_ne!(copy.agents[0], "my-advisor");
    assert!(home.path().join("agents").join(&copy.agents[0]).join("agent.toml").is_file());
}

#[tokio::test]
async fn replace_backs_up_the_earlier_import_and_reuses_its_names() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "舊版規則。");
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Fresh).await.0, None);

    // A dry run plans the same ids (not `-imported`) and says nothing was edited.
    let v2 = source(&src.path().join("v2"), "my-advisor", "新版規則。");
    let (code, plan) = run(home.path(), &v2, true, replace("my-advisor", false)).await;
    assert_eq!(code, None, "{plan}");
    assert_eq!(plan["details"]["replace"]["edited"], false);
    assert_eq!(plan["meta"]["slug"], "my-advisor");
    assert!(!plan.to_string().contains("my-advisor-imported"), "{plan}");

    let (code, done) = run(home.path(), &v2, false, replace("my-advisor", false)).await;
    assert_eq!(code, None, "{done}");
    let agent = home.path().join("agents/my-advisor");
    assert!(std::fs::read_to_string(agent.join("INSTRUCTIONS.md")).unwrap().contains("新版規則"));
    let trashed = trash_entries(home.path(), "my-advisor");
    assert_eq!(trashed.len(), 1, "earlier employee kept in the trash");
    let old = home.path().join("agents/_trash").join(&trashed[0]).join("INSTRUCTIONS.md");
    assert!(std::fs::read_to_string(old).unwrap().contains("舊版規則"));
    let backups: Vec<_> = std::fs::read_dir(home.path().join("experts/_replaced")).unwrap().flatten().collect();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path().join("record/install.json").is_file());
    assert_eq!(crate::expert::list_records(home.path()).len(), 1);
    assert_eq!(done["details"]["replaced"]["agents"][0], "my-advisor");
}

#[tokio::test]
async fn replacing_an_edited_employee_needs_explicit_consent() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "舊版規則。");
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Fresh).await.0, None);
    let agent = home.path().join("agents/my-advisor");
    std::fs::write(agent.join("SOUL.md"), "# 我改過的人格\n").unwrap();
    // A runtime-managed file changing does not count as an edit.
    std::fs::write(agent.join(".mcp.json"), "{}").unwrap();

    let (code, report) = run(home.path(), &pack, false, ReimportMode::Fresh).await;
    assert_eq!(code, Some("import.already_imported"));
    assert_eq!(report["error"]["params"]["edited"], true);

    let (code, plan) = run(home.path(), &pack, true, replace("my-advisor", false)).await;
    assert_eq!(code, None);
    let edits = &plan["details"]["replace"]["edits"];
    assert_eq!(edits[0]["agent"], "my-advisor");
    assert_eq!(edits[0]["changes"][0]["path"], "SOUL.md");
    assert_eq!(edits[0]["changes"][0]["change"], "modified");
    assert_eq!(edits[0]["changes"].as_array().unwrap().len(), 1, "{edits}");

    let (code, report) = run(home.path(), &pack, false, replace("my-advisor", false)).await;
    assert_eq!(code, Some("import.replace_edited"));
    assert_eq!(report["error"]["params"]["edits"][0]["agent"], "my-advisor");
    assert!(std::fs::read_to_string(agent.join("SOUL.md")).unwrap().contains("我改過的人格"), "nothing moved");
    assert!(trash_entries(home.path(), "my-advisor").is_empty());

    let (code, _) = run(home.path(), &pack, false, replace("my-advisor", true)).await;
    assert_eq!(code, None);
    let trashed = trash_entries(home.path(), "my-advisor");
    let kept = home.path().join("agents/_trash").join(&trashed[0]).join("SOUL.md");
    assert!(std::fs::read_to_string(kept).unwrap().contains("我改過的人格"), "the edit is in the backup");
}

#[tokio::test]
async fn a_failed_replacement_puts_the_earlier_import_back() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let first = source(src.path(), "my-advisor", "舊版規則。");
    assert_eq!(run(home.path(), &first, false, ReimportMode::Fresh).await.0, None);
    // A different pack named as the replacement of `my-advisor`.
    let other = source(src.path(), "other-pack", "別的。");
    let (code, _) = run(home.path(), &other, false, replace("my-advisor", false)).await;
    assert_eq!(code, Some("import.replace_mismatch"));
    // Checked before anything moved: the earlier import never left.
    assert!(!home.path().join("experts/_replaced").exists(), "nothing was moved to a backup");
    assert!(home.path().join("agents/my-advisor/agent.toml").is_file());
    assert!(crate::expert::read_record(home.path(), "my-advisor").is_some());
    assert!(!home.path().join("agents/other-pack").exists());
    assert!(trash_entries(home.path(), "my-advisor").is_empty());

    let (code, _) = run(home.path(), &other, false, replace("ghost", false)).await;
    assert_eq!(code, Some("import.replace_missing"));
}

#[tokio::test]
async fn the_agent_cap_counts_every_employee_before_anything_is_written() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let existing = home.path().join("agents/boss");
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::write(existing.join("agent.toml"), "[agent]\nname = \"boss\"\n").unwrap();
    let pack = source(src.path(), "my-advisor", "規則。");

    let rpt = tempfile::tempdir().unwrap();
    let path = rpt.path().join("r.json");
    let o = PipelineOpts { agent_cap: Some(1), report_json: Some(path.clone()), ..Default::default() };
    let err = cmd_install_with(home.path(), pack.to_str().unwrap(), o).await.unwrap_err();
    let f = failure_of(&err).unwrap();
    assert_eq!(f.code, "import.quota_exceeded");
    assert_eq!((f.params["max"].as_u64(), f.params["current"].as_u64(), f.params["adding"].as_u64()), (Some(1), Some(1), Some(1)));
    assert!(!home.path().join("agents/my-advisor").exists());
    assert!(!home.path().join("imported/my-advisor").exists(), "not even the original is archived");

    // Replacing an employee frees its seat: the same cap allows it.
    let o = PipelineOpts { agent_cap: Some(2), ..Default::default() };
    cmd_install_with(home.path(), pack.to_str().unwrap(), o).await.unwrap();
    let o = PipelineOpts { agent_cap: Some(2), dry_run: true, reimport: replace("my-advisor", false), ..Default::default() };
    cmd_install_with(home.path(), pack.to_str().unwrap(), o).await.unwrap();
}

#[tokio::test]
async fn nothing_imported_lists_each_employees_reason() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    // An employee with the same id that no import owns.
    let taken = home.path().join("agents/my-advisor");
    std::fs::create_dir_all(&taken).unwrap();
    std::fs::write(taken.join("agent.toml"), "[agent]\nname = \"my-advisor\"\n").unwrap();
    let pack = source(src.path(), "my-advisor", "規則。");

    for dry_run in [true, false] {
        let (code, report) = run(home.path(), &pack, dry_run, ReimportMode::Fresh).await;
        assert_eq!(code, Some("import.nothing_imported"), "dry_run={dry_run}");
        let agents = &report["error"]["params"]["agents"];
        assert_eq!(agents[0]["code"], "agent.conflict", "{report}");
        assert!(!report["error"]["message"].as_str().unwrap().contains("上方"));
    }
    // "Import as a copy" gets past the clash.
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Copy).await.0, None);
}

/// A source whose own names have no ASCII (a real 豆包 export is
/// all Chinese) gets its import name from the first employee's id. Before,
/// the second import saw that id taken, fell back to `<format>-import`, and
/// reported "nothing imported" instead of "imported before".
#[tokio::test]
async fn a_chinese_named_source_is_recognised_as_a_re_import() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let ws = src.path().join("戰略顧問");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("SOUL.md"), "# 我是戰略顧問\n").unwrap();
    let (code, first) = run(home.path(), &ws, false, ReimportMode::Fresh).await;
    assert_eq!(code, None, "{first}");
    let slug = first["meta"]["slug"].as_str().unwrap().to_string();

    let (code, again) = run(home.path(), &ws, true, ReimportMode::Fresh).await;
    assert_eq!(code, Some("import.already_imported"), "{again}");
    assert_eq!(again["error"]["params"]["slug"], slug.as_str());
    let (code, plan) = run(home.path(), &ws, true, replace(&slug, false)).await;
    assert_eq!(code, None, "{plan}");
    assert_eq!(plan["meta"]["slug"], slug.as_str());
}

// ── Replace with a team pack: checks before moving, failure part-way ──

struct Member<'a> {
    name: &'a str,
    reports_to: &'a str,
    department: &'a str,
}

/// A native pack `team-pack` with `members`, optionally a knowledge page
/// and a hook (which files an approval).
fn team_pack(dir: &Path, members: &[Member<'_>], wiki: bool, hook: bool) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let mut toml = String::from("[expert]\nname = \"team-pack\"\nversion = \"1.0.0\"\ndescription = \"團隊\"\n");
    for m in members {
        toml.push_str(&format!(
            "\n[[expert.agents]]\nname = \"{}\"\nrole = \"worker\"\nreports_to = \"{}\"\ndepartment = \"{}\"\n",
            m.name, m.reports_to, m.department
        ));
        let soul = dir.join("agents").join(m.name);
        std::fs::create_dir_all(&soul).unwrap();
        std::fs::write(soul.join("soul.md"), format!("# {}\n\n我負責整理資料。\n", m.name)).unwrap();
    }
    std::fs::write(dir.join("expert.toml"), toml).unwrap();
    if wiki {
        std::fs::create_dir_all(dir.join("wiki/team")).unwrap();
        std::fs::write(dir.join("wiki/team/notes.md"), "# 團隊筆記\n\n先讀資料。\n").unwrap();
    }
    if hook {
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::write(dir.join("hooks/pre.sh"), "#!/bin/sh\necho hi\n").unwrap();
    }
    dir.to_path_buf()
}

fn org_department(home: &Path, id: &str) -> Option<String> {
    duduclaw_core::org_store::load(home).get(id).map(|e| e.department.clone())
}

fn hooks_approval(record_dir: &Path) -> String {
    let raw = std::fs::read_to_string(record_dir.join("hooks-state.json")).unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    v["approval_id"].as_str().expect("an approval was filed").to_string()
}

async fn approval_status(home: &Path, id: &str) -> duduclaw_gateway::approval::ApprovalStatus {
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home).unwrap();
    broker
        .get(&duduclaw_gateway::approval::ApprovalId::from(id.to_string()))
        .await
        .unwrap()
        .expect("approval exists")
        .status
}

#[tokio::test]
async fn a_replace_over_the_agent_cap_moves_nothing() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let v1 = team_pack(&src.path().join("v1"), &[Member { name: "lead", reports_to: "", department: "" }], false, false);
    assert_eq!(run(home.path(), &v1, false, ReimportMode::Fresh).await.0, None);
    let three = [
        Member { name: "lead", reports_to: "", department: "" },
        Member { name: "helper", reports_to: "lead", department: "" },
        Member { name: "writer", reports_to: "lead", department: "" },
    ];
    let v2 = team_pack(&src.path().join("v2"), &three, false, false);
    let o = PipelineOpts { agent_cap: Some(2), reimport: replace("team-pack", false), ..Default::default() };
    let err = cmd_install_with(home.path(), v2.to_str().unwrap(), o).await.unwrap_err();
    assert_eq!(failure_of(&err).unwrap().code, "import.quota_exceeded");
    assert!(!home.path().join("experts/_replaced").exists(), "nothing was moved to a backup");
    assert!(home.path().join("agents/lead/agent.toml").is_file());
    assert!(trash_entries(home.path(), "lead").is_empty());
}

/// The new version fails after it wrote employees, org entries, a page and
/// a hook approval: all of that is moved aside / undone, the earlier
/// version is back with its org entry, and its own approval still waits.
#[tokio::test]
async fn a_replacement_failing_part_way_is_undone_completely() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let v1 = team_pack(&src.path().join("v1"), &[Member { name: "lead", reports_to: "", department: "銷售" }], false, true);
    assert_eq!(run(home.path(), &v1, false, ReimportMode::Fresh).await.0, None);
    let old_record = crate::expert::read_record(home.path(), "team-pack").unwrap();
    let old_approval = hooks_approval(&home.path().join("experts/team-pack"));
    assert_eq!(org_department(home.path(), "lead").as_deref(), Some("銷售"));

    let v2 = team_pack(
        &src.path().join("v2"),
        &[
            Member { name: "lead", reports_to: "", department: "客服" },
            Member { name: "helper", reports_to: "lead", department: "客服" },
        ],
        true,
        true,
    );
    let rpt = tempfile::tempdir().unwrap();
    let path = rpt.path().join("r.json");
    let o = PipelineOpts {
        reimport: replace("team-pack", false),
        report_json: Some(path.clone()),
        fail_after_dispatch: true,
        ..Default::default()
    };
    let err = cmd_install_with(home.path(), v2.to_str().unwrap(), o).await.unwrap_err();
    assert_eq!(failure_of(&err).unwrap().code, "import.replace_failed");
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(report["details"]["rolled_back"]["withdrawn_approvals"], 1, "{report}");

    // The earlier version is back, as it was.
    let lead = std::fs::read_to_string(home.path().join("agents/lead/agent.toml")).unwrap();
    assert!(lead.contains("銷售") && !lead.contains("客服"), "{lead}");
    assert_eq!(org_department(home.path(), "lead").as_deref(), Some("銷售"), "org entry restored");
    let rec = crate::expert::read_record(home.path(), "team-pack").unwrap();
    assert_eq!(rec.installed_at, old_record.installed_at);
    assert!(trash_entries(home.path(), "lead").is_empty());
    // What only the new version created is gone from the live home.
    assert!(!home.path().join("agents/helper").exists());
    assert_eq!(org_department(home.path(), "helper"), None, "new org entry dropped");
    assert!(!home.path().join("shared/wiki/team/notes.md").exists());
    // …and kept in the backup's failed-new/.
    let backups: Vec<_> = std::fs::read_dir(home.path().join("experts/_replaced")).unwrap().flatten().collect();
    assert_eq!(backups.len(), 1);
    let failed = backups[0].path().join("failed-new");
    assert!(failed.join("agents/helper/agent.toml").is_file());
    assert!(failed.join("agents/lead/agent.toml").is_file());
    assert!(failed.join("shared/wiki/team/notes.md").is_file());
    // The failed version's approval is withdrawn; the earlier one still waits.
    let new_approval = hooks_approval(&failed.join("experts/team-pack"));
    use duduclaw_gateway::approval::ApprovalStatus;
    assert_eq!(approval_status(home.path(), &new_approval).await, ApprovalStatus::Denied);
    assert_eq!(approval_status(home.path(), &old_approval).await, ApprovalStatus::Pending);

    // A replace that succeeds withdraws the earlier version's approval.
    let (code, done) = run(home.path(), &v2, false, replace("team-pack", false)).await;
    assert_eq!(code, None, "{done}");
    assert_eq!(done["details"]["replaced"]["withdrawn_approvals"], 1, "{done}");
    assert_eq!(approval_status(home.path(), &old_approval).await, ApprovalStatus::Denied);
    assert_eq!(org_department(home.path(), "lead").as_deref(), Some("客服"));
    assert!(home.path().join("agents/helper/agent.toml").is_file());
}

/// An id in the record whose directory was created after that import
/// (the employee was removed, someone made a new one with the same id) is
/// not the import's: a replace leaves it alone.
#[tokio::test]
async fn a_replace_leaves_an_employee_that_only_reuses_the_id() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "規則。");
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Fresh).await.0, None);
    // Make the import look old, then recreate the employee "now".
    let rec_path = home.path().join("experts/my-advisor/install.json");
    let mut rec: Value = serde_json::from_str(&std::fs::read_to_string(&rec_path).unwrap()).unwrap();
    rec["installed_at"] = Value::from("2020-01-01T00:00:00+00:00");
    std::fs::write(&rec_path, rec.to_string()).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
    std::fs::File::options().write(true).open(&rec_path).unwrap().set_modified(old).unwrap();
    let agent = home.path().join("agents/my-advisor");
    std::fs::remove_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"), "[agent]\nname = \"my-advisor\"\n").unwrap();
    std::fs::write(agent.join("MINE.md"), "別人的員工\n").unwrap();
    if std::fs::metadata(&agent).and_then(|m| m.created()).is_err() {
        eprintln!("file system without creation times: skipped");
        return;
    }

    let (code, plan) = run(home.path(), &pack, true, replace("my-advisor", false)).await;
    assert_eq!(plan["details"]["replace"]["kept"][0], "my-advisor", "{plan} {code:?}");
    let _ = run(home.path(), &pack, false, replace("my-advisor", true)).await;
    assert!(agent.join("MINE.md").is_file(), "the other employee was not moved");
    assert!(trash_entries(home.path(), "my-advisor").is_empty());
}

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

/// The installer is killed half way through a replace (the dashboard's
/// timeout, the machine going down): the journal it left makes the next
/// import put the earlier version back first, with an audit event.
#[tokio::test]
async fn a_replace_stopped_half_way_is_put_back_by_the_next_import() {
    use duduclaw_gateway::approval::ApprovalStatus;
    use duduclaw_gateway::expert_replace_journal as journal;
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let v1 = team_pack(&src.path().join("v1"), &[Member { name: "lead", reports_to: "", department: "銷售" }], false, true);
    assert_eq!(run(home.path(), &v1, false, ReimportMode::Fresh).await.0, None);
    let old_approval = hooks_approval(&home.path().join("experts/team-pack"));
    let v2 = team_pack(
        &src.path().join("v2"),
        &[
            Member { name: "lead", reports_to: "", department: "客服" },
            Member { name: "helper", reports_to: "lead", department: "客服" },
        ],
        true,
        true,
    );
    let o = PipelineOpts { reimport: replace("team-pack", false), abandon_after_dispatch: true, ..Default::default() };
    cmd_install_with(home.path(), v2.to_str().unwrap(), o).await.unwrap_err();
    // Half done: the new version's files are live, the journal is left.
    let left = journal::list(home.path());
    assert_eq!(left.len(), 1);
    assert!(home.path().join("agents/helper/agent.toml").is_file());
    let new_approval = hooks_approval(&home.path().join("experts/team-pack"));
    // Its process is gone.
    let mut j = left[0].clone();
    j.pid = dead_pid();
    j.save(home.path()).unwrap();

    let other = source(src.path(), "other-pack", "別的。");
    let (code, report) = run(home.path(), &other, true, ReimportMode::Fresh).await;
    assert_eq!(code, None, "{report}");
    assert_eq!(report["details"]["recovered"][0], "team-pack", "{report}");

    assert!(journal::list(home.path()).is_empty(), "journal removed");
    let lead = std::fs::read_to_string(home.path().join("agents/lead/agent.toml")).unwrap();
    assert!(lead.contains("銷售") && !lead.contains("客服"), "{lead}");
    assert_eq!(org_department(home.path(), "lead").as_deref(), Some("銷售"));
    assert!(!home.path().join("agents/helper").exists());
    assert_eq!(org_department(home.path(), "helper"), None);
    assert!(!home.path().join("shared/wiki/team/notes.md").exists());
    assert!(trash_entries(home.path(), "lead").is_empty());
    assert_eq!(approval_status(home.path(), &old_approval).await, ApprovalStatus::Pending);
    assert_eq!(approval_status(home.path(), &new_approval).await, ApprovalStatus::Denied);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("expert_replace_recovered"), "{audit}");
}

/// A replace that completes or rolls back leaves no journal behind.
#[tokio::test]
async fn a_finished_replace_leaves_no_journal() {
    use duduclaw_gateway::expert_replace_journal as journal;
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let pack = source(src.path(), "my-advisor", "規則。");
    assert_eq!(run(home.path(), &pack, false, ReimportMode::Fresh).await.0, None);
    assert_eq!(run(home.path(), &pack, false, replace("my-advisor", false)).await.0, None);
    assert!(journal::list(home.path()).is_empty());
    let o = PipelineOpts { reimport: replace("my-advisor", false), fail_after_dispatch: true, ..Default::default() };
    cmd_install_with(home.path(), pack.to_str().unwrap(), o).await.unwrap_err();
    assert!(journal::list(home.path()).is_empty());
    assert!(home.path().join("agents/my-advisor/agent.toml").is_file());
}
