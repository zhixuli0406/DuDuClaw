use super::*;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn setup() -> (TempDir, PathBuf) {
    let home = TempDir::new().unwrap();
    let skills = home.path().join("agents").join("alice").join("SKILLS");
    std::fs::create_dir_all(&skills).unwrap();
    (home, skills)
}

#[test]
fn locate_follows_loader_naming() {
    let (_home, skills) = setup();
    write(&skills, "flat.md", "x");
    write(&skills, "pdf/SKILL.md", "x");
    write(&skills, "pdf/references/api.md", "x");
    write(&skills, "group/nested/SKILL.md", "x");
    assert_eq!(locate(&skills, "flat").unwrap(), vec![PathBuf::from("flat.md")]);
    assert_eq!(locate(&skills, "PDF").unwrap(), vec![PathBuf::from("pdf")]);
    assert_eq!(locate(&skills, "nested").unwrap(), vec![PathBuf::from("group/nested")]);
    // Reference material is not a skill.
    assert!(locate(&skills, "api").unwrap().is_empty());
    assert!(locate(&skills, "missing").unwrap().is_empty());
}

#[test]
fn archive_moves_both_layouts_and_restore_puts_them_back() {
    let (home, skills) = setup();
    write(&skills, "pdf.md", "legacy");
    write(&skills, "pdf/SKILL.md", "spec");
    write(&skills, "pdf/scripts/run.py", "print()");
    write(&skills, "other.md", "keep");
    let m = archive(home.path(), "alice", &skills, "pdf", "user-1").unwrap();
    assert_eq!(m.entries, vec!["pdf", "pdf.md"]);
    assert!(!skills.join("pdf").exists() && !skills.join("pdf.md").exists());
    assert!(skills.join("other.md").exists());
    let listed = list(home.path(), "alice").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].skill_name, "pdf");
    assert_eq!(listed[0].archived_by, "user-1");

    restore(home.path(), "alice", &skills, &m.archive_id).unwrap();
    assert_eq!(std::fs::read_to_string(skills.join("pdf/SKILL.md")).unwrap(), "spec");
    assert_eq!(std::fs::read_to_string(skills.join("pdf/scripts/run.py")).unwrap(), "print()");
    assert_eq!(std::fs::read_to_string(skills.join("pdf.md")).unwrap(), "legacy");
    assert!(list(home.path(), "alice").unwrap().is_empty());
}

#[test]
fn restore_refuses_to_overwrite() {
    let (home, skills) = setup();
    write(&skills, "pdf.md", "old");
    let m = archive(home.path(), "alice", &skills, "pdf", "u").unwrap();
    write(&skills, "pdf.md", "new");
    let err = restore(home.path(), "alice", &skills, &m.archive_id).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(std::fs::read_to_string(skills.join("pdf.md")).unwrap(), "new");
    assert_eq!(list(home.path(), "alice").unwrap().len(), 1);
}

#[test]
fn archive_refuses_unknown_and_nested_skill_dirs() {
    let (home, skills) = setup();
    assert!(archive(home.path(), "alice", &skills, "nope", "u").is_err());
    write(&skills, "outer/SKILL.md", "x");
    write(&skills, "outer/inner/SKILL.md", "x");
    let err = archive(home.path(), "alice", &skills, "outer", "u").unwrap_err();
    assert!(err.contains("contains other skills"), "{err}");
    assert!(skills.join("outer/SKILL.md").exists());
}

#[test]
fn restore_rejects_bad_ids_and_other_employees() {
    let (home, skills) = setup();
    write(&skills, "pdf.md", "x");
    let m = archive(home.path(), "alice", &skills, "pdf", "u").unwrap();
    assert!(restore(home.path(), "alice", &skills, "../x").is_err());
    assert!(restore(home.path(), "bob", &skills, &m.archive_id).is_err());
}

#[test]
fn unsafe_manifest_entries_are_refused() {
    assert!(safe_relative("../etc").is_err());
    assert!(safe_relative("/abs").is_err());
    assert!(safe_relative("").is_err());
    assert!(safe_relative("group/nested").is_ok());
}

#[cfg(unix)]
#[test]
fn restore_refuses_a_parent_swapped_for_a_link() {
    let (home, skills) = setup();
    write(&skills, "group/foo/SKILL.md", "x");
    let m = archive(home.path(), "alice", &skills, "foo", "u").unwrap();
    assert_eq!(m.entries, vec!["group/foo"]);
    // The employee swaps the now-empty `group` for a link to somewhere else.
    let outside = home.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(skills.join("group")).unwrap();
    std::os::unix::fs::symlink(&outside, skills.join("group")).unwrap();
    let err = restore(home.path(), "alice", &skills, &m.archive_id).unwrap_err();
    assert!(err.contains("link"), "{err}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing lands outside");
    assert_eq!(list(home.path(), "alice").unwrap().len(), 1, "the archive is kept");
}

#[cfg(unix)]
#[test]
fn archive_and_restore_refuse_a_linked_skills_folder() {
    let (home, skills) = setup();
    write(&skills, "pdf.md", "x");
    let m = archive(home.path(), "alice", &skills, "pdf", "u").unwrap();
    let elsewhere = home.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    write(&elsewhere, "other.md", "x");
    std::fs::remove_dir_all(&skills).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &skills).unwrap();
    assert!(restore(home.path(), "alice", &skills, &m.archive_id).unwrap_err().contains("link"));
    assert!(archive(home.path(), "alice", &skills, "other", "u").unwrap_err().contains("link"));
    assert!(elsewhere.join("other.md").exists());
    assert!(!elsewhere.join("pdf.md").exists());
}

#[cfg(unix)]
#[test]
fn archive_refuses_a_skill_reached_through_a_linked_folder() {
    let (home, skills) = setup();
    write(&skills, "real/foo/SKILL.md", "x");
    std::os::unix::fs::symlink(skills.join("real"), skills.join("alias")).unwrap();
    // `foo` is found both directly and through `alias`; the linked path is refused.
    let err = archive(home.path(), "alice", &skills, "foo", "u").unwrap_err();
    assert!(err.contains("link"), "{err}");
    assert!(skills.join("real/foo/SKILL.md").exists());
}
