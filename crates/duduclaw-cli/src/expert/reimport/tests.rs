//! Re-import (replace / copy), the agent cap and "nothing imported" through
//! the real pipeline, in isolated temp homes.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ReimportMode;
use crate::expert::import_error::failure_of;
use crate::expert::install::{PipelineOpts, cmd_install_with};

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
