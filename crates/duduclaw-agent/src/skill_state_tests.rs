use super::*;
use tempfile::TempDir;

fn skill(name: &str) -> SkillFile {
    SkillFile { name: name.to_string(), content: String::new(), modified_unix: None }
}

#[test]
fn missing_file_disables_nothing() {
    let home = TempDir::new().unwrap();
    let state = read_disabled(home.path(), "alice");
    assert_eq!(state, DisabledSkills::none());
    assert!(!state.is_disabled("pdf"));
}

#[test]
fn set_and_clear_round_trip() {
    let home = TempDir::new().unwrap();
    let names = vec!["pdf".to_string(), "報價".to_string()];
    let after = set_disabled(home.path(), "alice", &names, true).unwrap();
    assert!(after.is_disabled("pdf") && after.is_disabled("報價"));
    assert_eq!(read_disabled(home.path(), "alice"), after);
    // Another employee is unaffected.
    assert!(!read_disabled(home.path(), "bob").is_disabled("pdf"));
    let after = set_disabled(home.path(), "alice", &["pdf".to_string()], false).unwrap();
    assert!(!after.is_disabled("pdf"));
    assert!(after.is_disabled("報價"));
}

#[test]
fn filter_only_drops_switched_off_names() {
    let home = TempDir::new().unwrap();
    set_disabled(home.path(), "alice", &["b".to_string()], true).unwrap();
    let state = read_disabled(home.path(), "alice");
    let kept: Vec<String> =
        state.filter(vec![skill("a"), skill("b"), skill("c")]).into_iter().map(|s| s.name).collect();
    assert_eq!(kept, vec!["a", "c"]);
}

#[test]
fn corrupt_file_disables_everything_and_refuses_writes() {
    let home = TempDir::new().unwrap();
    let path = state_path(home.path(), "alice").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let state = read_disabled(home.path(), "alice");
    assert!(state.error().is_some());
    assert!(state.is_disabled("anything"));
    assert!(state.filter(vec![skill("a")]).is_empty());
    let err = set_disabled(home.path(), "alice", &["a".to_string()], false).unwrap_err();
    assert!(err.contains("cannot be read"), "{err}");
    // The corrupt file was not replaced.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn unknown_version_is_unreadable() {
    let home = TempDir::new().unwrap();
    let path = state_path(home.path(), "alice").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"version":99,"disabled":[]}"#).unwrap();
    assert!(read_disabled(home.path(), "alice").error().is_some());
}

#[test]
fn invalid_agent_id_is_unreadable_and_unwritable() {
    let home = TempDir::new().unwrap();
    assert!(read_disabled(home.path(), "../evil").is_disabled("x"));
    assert!(set_disabled(home.path(), "../evil", &["x".to_string()], true).is_err());
}

#[test]
fn skill_names_are_validated() {
    assert!(validate_skill_name("pdf").is_ok());
    assert!(validate_skill_name("中文技能").is_ok());
    for bad in ["", " pdf", "a/b", "a\\b", "..", ".hidden", "a\0b"] {
        assert!(validate_skill_name(bad).is_err(), "{bad:?}");
    }
    let home = TempDir::new().unwrap();
    assert!(set_disabled(home.path(), "alice", &["a/b".to_string()], true).is_err());
}

#[test]
fn switches_ignore_ascii_case() {
    let home = TempDir::new().unwrap();
    set_disabled(home.path(), "alice", &["quote".to_string()], true).unwrap();
    let state = set_disabled(home.path(), "alice", &["QUOTE".to_string()], true).unwrap();
    assert_eq!(state.names(), vec!["quote"], "no duplicate entry");
    assert!(state.is_disabled("Quote"));
    let state = set_disabled(home.path(), "alice", &["Quote".to_string()], false).unwrap();
    assert!(!state.is_disabled("quote"));
}

#[test]
fn write_errors_do_not_reveal_paths() {
    let home = TempDir::new().unwrap();
    let path = state_path(home.path(), "alice").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "bad").unwrap();
    let err = set_disabled(home.path(), "alice", &["a".to_string()], true).unwrap_err();
    assert!(!err.contains(&home.path().display().to_string()), "{err}");
}
