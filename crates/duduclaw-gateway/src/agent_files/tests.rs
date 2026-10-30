//! The profile file writer on its own: concurrency, conflicts, path safety,
//! permissions, history and baselines.
use super::writer::{Baseline, Commit, CommitError, CommitMeta, commit_locked};
use super::*;

fn home_with_agent() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join("agents/a1")).unwrap();
    std::fs::write(home.join("agents/a1/agent.toml"), "[agent]\nname = \"a1\"\n").unwrap();
    std::fs::write(home.join("agents/a1/SOUL.md"), "v0").unwrap();
    (dir, home)
}

fn commit<'a>(home: &'a Path, key: FileKey, expected: Option<&'a str>) -> Commit<'a> {
    Commit {
        home,
        agent_id: "a1",
        key,
        expected_hash: expected,
        meta: CommitMeta::new("u1", "dashboard"),
        baseline: Baseline::Accept,
    }
}

#[test]
fn base_hash_conflict_and_concurrent_saves_only_one_wins() {
    let (_d, home) = home_with_agent();
    let base = hash_of("v0");
    let stale = commit_locked(&commit(&home, FileKey::Soul, Some("nope")), |_| Ok("x".into()), || Ok(()));
    assert!(matches!(stale, Err(CommitError::Conflict { .. })));
    // Many writers race from the same base: exactly one applies.
    let wins = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let (home, base, wins) = (home.clone(), base.clone(), wins.clone());
            std::thread::spawn(move || {
                let r = commit_locked(&commit(&home, FileKey::Soul, Some(&base)), move |_| Ok(format!("v{}", i + 1)), || Ok(()));
                if r.is_ok() {
                    wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn guard_and_render_refusals_write_nothing() {
    let (_d, home) = home_with_agent();
    let r = commit_locked(&commit(&home, FileKey::Soul, None), |_| Ok("new".into()), || Err("permission denied".into()));
    assert_eq!(r.unwrap_err(), CommitError::Refused("permission denied".into()));
    let r = commit_locked(&commit(&home, FileKey::Soul, None), |_| Err("bad".into()), || Ok(()));
    assert!(matches!(r, Err(CommitError::Refused(_))));
    let r = commit_locked(&commit(&home, FileKey::Soul, None), |_| Ok("x".repeat(MAX_MARKDOWN_BYTES + 1)), || Ok(()));
    assert!(matches!(r, Err(CommitError::Refused(_))));
    assert_eq!(std::fs::read_to_string(home.join("agents/a1/SOUL.md")).unwrap(), "v0");
    assert!(history::list(&home, "a1", &FileKey::Soul).is_empty());
}

#[test]
fn writes_are_owner_only_versioned_and_baselined() {
    let (_d, home) = home_with_agent();
    let o = commit_locked(&commit(&home, FileKey::Soul, Some(&hash_of("v0"))), |_| Ok("v1".into()), || Ok(())).unwrap();
    assert!(!o.unchanged && o.version_id.is_some());
    let path = home.join("agents/a1/SOUL.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(!home.join("agents/a1/SOUL.md.tmp").exists() && !home.join("agents/a1/SOUL.toml.tmp").exists());
    let sources: Vec<String> = history::list(&home, "a1", &FileKey::Soul).into_iter().map(|v| v.source).collect();
    assert_eq!(sources, vec!["dashboard", "initial"]);
    let dir = home.join("agents/a1");
    assert_eq!(
        duduclaw_security::soul_guard::doc_drift(duduclaw_security::soul_guard::ProtectedDoc::Soul, &dir),
        duduclaw_security::soul_guard::DocDrift::Intact
    );
    // The same content again is a no-op: no version, no audit.
    let o = commit_locked(&commit(&home, FileKey::Soul, None), |_| Ok("v1".into()), || Ok(())).unwrap();
    assert!(o.unchanged);
    assert_eq!(history::list(&home, "a1", &FileKey::Soul).len(), 2);
    let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    assert_eq!(audit.matches("agent_file_changed").count(), 1);
    assert!(!audit.contains("\"v1\""), "the audit row carries hashes, not content");
}

#[test]
fn paths_refuse_symlinks_escapes_and_bad_ids() {
    let (_d, home) = home_with_agent();
    for bad in ["../x", ".hidden", "_trash", "a/b", ""] {
        assert!(agent_dir(&home, bad).is_err(), "{bad}");
    }
    assert!(agent_dir(&home, "missing").is_err());
    #[cfg(unix)]
    {
        let outside = home.join("outside.md");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, home.join("agents/a1/IDENTITY.md")).unwrap();
        assert!(file_path(&home, "a1", &FileKey::Identity).is_err(), "a symlinked file is refused");
        let r = commit_locked(&commit(&home, FileKey::Identity, None), |_| Ok("x".into()), || Ok(()));
        assert!(matches!(r, Err(CommitError::Refused(_))));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        std::fs::create_dir_all(home.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere"), home.join("agents/linked")).unwrap();
        assert!(agent_dir(&home, "linked").is_err(), "a symlinked employee directory is refused");
    }
    // A missing INSTRUCTIONS.md may be created; a missing agent.toml may not.
    assert!(file_path(&home, "a1", &FileKey::Instructions).is_ok());
    std::fs::create_dir_all(home.join("agents/empty")).unwrap();
    assert!(file_path(&home, "empty", &FileKey::AgentToml).is_err());
}

#[test]
fn config_reads_and_never_turns_the_failure_rule_off() {
    let t: toml::Table = "history_keep = 1\nreview_daily_limit = 5\non_analysis_failure = \"pass\"\nreview_model = \" m \"\n".parse().unwrap();
    let c = AgentFilesConfig::from_table(&t);
    assert_eq!(c.history_keep, 5, "clamped to 5..=100");
    assert_eq!(c.review_daily_limit, 5);
    assert_eq!(c.on_analysis_failure, verdict::Verdict::Warn, "only warn or block");
    assert_eq!(c.review_model.as_deref(), Some("m"));
    let t: toml::Table = "on_analysis_failure = \"block\"\n".parse().unwrap();
    assert_eq!(AgentFilesConfig::from_table(&t).on_analysis_failure, verdict::Verdict::Block);
}

#[test]
fn skill_files_are_written_in_place_with_history_and_never_created() {
    let (_d, home) = home_with_agent();
    let skills = home.join("agents/a1/SKILLS/quote");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("SKILL.md"), "---\nname: quote\n---\nv0").unwrap();
    std::fs::write(skills.join("run.py"), "print(1)").unwrap();
    let key = FileKey::parse("skill:quote/SKILL.md").unwrap();
    assert_eq!(key.as_str(), "skill:quote/SKILL.md");
    assert_eq!((key.ext(), key.max_bytes(), key.label_key().as_str()), ("md", MAX_MARKDOWN_BYTES, "agentFiles.file.skill"));
    assert!(key.text_editable() && key.protected_doc().is_none());
    let o = commit_locked(&commit(&home, key.clone(), Some(&hash_of("---\nname: quote\n---\nv0"))), |_| Ok("---\nname: quote\n---\nv1".into()), || Ok(()))
        .unwrap();
    assert!(o.version_id.is_some());
    assert_eq!(std::fs::read_to_string(skills.join("SKILL.md")).unwrap(), "---\nname: quote\n---\nv1");
    assert_eq!(std::fs::read_to_string(skills.join("run.py")).unwrap(), "print(1)", "scripts untouched");
    let rows = history::list(&home, "a1", &key);
    assert_eq!(rows.iter().map(|r| r.source.as_str()).collect::<Vec<_>>(), vec!["dashboard", "initial"]);
    assert_eq!(rows[0].file, "skill:quote/SKILL.md");
    assert_eq!(history::read_version(&home, "a1", &key, &rows[1].version_id).unwrap(), "---\nname: quote\n---\nv0");
    assert!(home.join("agent_file_history/a1").join(key.history_dir()).is_dir(), "kept outside the agent folder");
    assert!(home.join("agent_file_history/a1/locks").join(format!("{}.lock", key.history_dir())).exists(), "the lock lives outside");
    assert!(!skills.join("SKILL.md.lock").exists(), "no lock file in the employee's folder");
    // Leftovers the skill loader must never pick up as skills.
    let stray: Vec<String> = std::fs::read_dir(&skills)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md") && n != "SKILL.md")
        .collect();
    assert!(stray.is_empty(), "{stray:?}");
    // A skill removed after the caller read it is not recreated.
    let gone = home.join("agents/a1/SKILLS/gone");
    std::fs::create_dir_all(&gone).unwrap();
    std::fs::write(gone.join("SKILL.md"), "").unwrap();
    let gone_key = FileKey::parse("skill:gone/SKILL.md").unwrap();
    let empty = hash_of("");
    let c = commit(&home, gone_key, Some(&empty));
    std::fs::remove_file(gone.join("SKILL.md")).unwrap();
    let r = commit_locked(&c, |_| Ok("x".into()), || Ok(()));
    assert!(matches!(r, Err(CommitError::Refused(_))), "{r:?}");
    assert!(!gone.join("SKILL.md").exists());
    // A skill that does not exist is not created through this path.
    let missing = FileKey::parse("skill:new/SKILL.md").unwrap();
    let r = commit_locked(&commit(&home, missing, None), |_| Ok("x".into()), || Ok(()));
    assert!(matches!(r, Err(CommitError::Refused(_))), "{r:?}");
    assert!(!home.join("agents/a1/SKILLS/new").exists());
    // Traversal never parses.
    for bad in ["skill:../SOUL.md", "skill:quote/../../SOUL.md", "skill:quote/run.py", "skill:", "skills:quote/SKILL.md"] {
        assert!(FileKey::parse(bad).is_err(), "{bad}");
    }
}

#[cfg(unix)]
#[test]
fn a_skill_folder_swapped_for_a_symlink_while_saving_is_not_followed() {
    let (_d, home) = home_with_agent();
    let skills = home.join("agents/a1/SKILLS/quote");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("SKILL.md"), "v0").unwrap();
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("SKILL.md"), "v0").unwrap();
    let key = FileKey::parse("skill:quote/SKILL.md").unwrap();
    let (skills_dir, outside) = (home.join("agents/a1/SKILLS"), elsewhere.clone());
    // The swap happens after the in-lock check, while the new text is rendered.
    let r = commit_locked(
        &commit(&home, key, Some(&hash_of("v0"))),
        move |_| {
            std::fs::rename(skills_dir.join("quote"), skills_dir.join("quote-old")).unwrap();
            std::os::unix::fs::symlink(&outside, skills_dir.join("quote")).unwrap();
            Ok("v1".into())
        },
        || Ok(()),
    );
    assert!(matches!(r, Err(CommitError::Refused(_))), "{r:?}");
    assert_eq!(std::fs::read_to_string(elsewhere.join("SKILL.md")).unwrap(), "v0", "nothing written through the link");
    assert!(!elsewhere.join("SKILL.md.tmp").exists());
}

// Added regression test; not run in this change.
#[tokio::test]
async fn agent_channel_write_fences_disconnect_and_refuses_unreadable_memory() {
    let (_dir, home) = home_with_agent();
    let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    engine.record_connector_disconnect("channel:telegram:a1").await.unwrap();
    let next = "[agent]\nname='a1'\n[channels.telegram]\nbot_token='test-token'\n";
    commit_locked(&commit(&home, FileKey::AgentToml, None), |_| Ok(next.into()), || Ok(())).unwrap();
    assert!(engine.disconnected_connector_refs().await.unwrap().is_empty());
    let (_bad_dir, bad_home) = home_with_agent();
    let path = bad_home.join("agents/a1/agent.toml");
    let before = std::fs::read_to_string(&path).unwrap();
    std::fs::write(bad_home.join("memory.db"), "not a database").unwrap();
    let result = commit_locked(&commit(&bad_home, FileKey::AgentToml, None), |_| Ok(next.into()), || Ok(()));
    assert!(matches!(result, Err(CommitError::Refused(_))));
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn user_character_limit_and_profile_baselines() {
    let (_d, home) = home_with_agent();
    for key in [FileKey::User, FileKey::Identity] {
        let content = "字".repeat(MAX_USER_CHARS);
        let out = commit_locked(&commit(&home, key.clone(), None), |_| Ok(content.clone()), || Ok(())).unwrap();
        assert!(!out.unchanged);
        let doc = key.protected_doc().expect("profile files have baselines");
        assert_eq!(duduclaw_security::soul_guard::doc_drift(doc, &home.join("agents/a1")), duduclaw_security::soul_guard::DocDrift::Intact);
    }
    let before = std::fs::read_to_string(home.join("agents/a1/USER.md")).unwrap();
    let result = commit_locked(&commit(&home, FileKey::User, None), |_| Ok("字".repeat(MAX_USER_CHARS + 1)), || Ok(()));
    assert!(matches!(result, Err(CommitError::Refused(_))));
    assert_eq!(std::fs::read_to_string(home.join("agents/a1/USER.md")).unwrap(), before);
}

#[test]
fn user_writes_mask_before_hash_history_disk_and_baseline() {
    let (_d, home) = home_with_agent();
    let raw = "password: hunter2";
    let masked = duduclaw_security::credential_detect::mask_document(raw).unwrap();
    let result = commit_locked(&commit(&home, FileKey::User, None), |_| Ok(raw.into()), || Ok(())).unwrap();
    assert_eq!(result.after, masked);
    assert_eq!(result.hash, hash_of(&masked));
    assert_eq!(std::fs::read_to_string(home.join("agents/a1/USER.md")).unwrap(), masked);
    assert_eq!(duduclaw_security::soul_guard::doc_drift(
        duduclaw_security::soul_guard::ProtectedDoc::User, &home.join("agents/a1")),
        duduclaw_security::soul_guard::DocDrift::Intact);
    let versions = history::list(&home, "a1", &FileKey::User);
    for version in versions {
        let content = history::read_version(&home, "a1", &FileKey::User, &version.version_id).unwrap();
        assert!(!content.contains("hunter2"));
    }
    let again = commit_locked(&commit(&home, FileKey::User, None), |_| Ok(raw.into()), || Ok(())).unwrap();
    assert!(again.unchanged);
}
