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
    assert!(history::list(&home, "a1", FileKey::Soul).is_empty());
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
    let sources: Vec<String> = history::list(&home, "a1", FileKey::Soul).into_iter().map(|v| v.source).collect();
    assert_eq!(sources, vec!["dashboard", "initial"]);
    let dir = home.join("agents/a1");
    assert_eq!(
        duduclaw_security::soul_guard::doc_drift(duduclaw_security::soul_guard::ProtectedDoc::Soul, &dir),
        duduclaw_security::soul_guard::DocDrift::Intact
    );
    // The same content again is a no-op: no version, no audit.
    let o = commit_locked(&commit(&home, FileKey::Soul, None), |_| Ok("v1".into()), || Ok(())).unwrap();
    assert!(o.unchanged);
    assert_eq!(history::list(&home, "a1", FileKey::Soul).len(), 2);
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
        assert!(file_path(&home, "a1", FileKey::Identity).is_err(), "a symlinked file is refused");
        let r = commit_locked(&commit(&home, FileKey::Identity, None), |_| Ok("x".into()), || Ok(()));
        assert!(matches!(r, Err(CommitError::Refused(_))));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
        std::fs::create_dir_all(home.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere"), home.join("agents/linked")).unwrap();
        assert!(agent_dir(&home, "linked").is_err(), "a symlinked employee directory is refused");
    }
    // A missing INSTRUCTIONS.md may be created; a missing agent.toml may not.
    assert!(file_path(&home, "a1", FileKey::Instructions).is_ok());
    std::fs::create_dir_all(home.join("agents/empty")).unwrap();
    assert!(file_path(&home, "empty", FileKey::AgentToml).is_err());
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
