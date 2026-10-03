//! G1 round 2 (M1): the Write/Edit lane resolves symbolic links.
//!
//! The allow-list compared literal paths only, so an agent could create a
//! link inside its own directory pointing at `<home>` state and write
//! through it. These cases build a real home in a temporary directory.
#![cfg(unix)]

use super::*;
use std::os::unix::fs::symlink;

struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn fixture() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".duduclaw");
    for d in ["agents/me", "agents/ceo", "evals/me", "evals/ceo"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("agents/me/SOUL.md"), "soul").unwrap();
    std::fs::write(root.join("agents/me/CONTRACT.toml"), "").unwrap();
    std::fs::write(root.join("tool_calls.jsonl"), "").unwrap();
    Home { _dir: dir, root }
}

fn me() -> HookCaller {
    agent("me")
}

#[test]
fn link_in_own_dir_to_home_state_is_refused() {
    let h = fixture();
    symlink(h.root.join("evals/me"), h.root.join("agents/me/ev")).unwrap();
    let p = h.root.join("agents/me/ev/case.toml");
    let d = check_caller_scope(&p, &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
    // The plain own-directory write next to it is still fine.
    let d = check_caller_scope(&h.root.join("agents/me/notes.md"), &h.root, &me());
    assert!(d.is_allowed(), "{d:?}");
}

#[test]
fn link_to_a_file_is_resolved_too() {
    let h = fixture();
    symlink(h.root.join("tool_calls.jsonl"), h.root.join("agents/me/log.txt")).unwrap();
    let d = check_caller_scope(&h.root.join("agents/me/log.txt"), &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
}

#[test]
fn dotdot_after_a_link_follows_the_link() {
    // Lexically `agents/me/ev/../ceo/x` is `agents/me/ceo/x` (own); the
    // kernel resolves `ev` first, so it is really `evals/ceo/x`.
    let h = fixture();
    symlink(h.root.join("evals/me"), h.root.join("agents/me/ev")).unwrap();
    let p = h.root.join("agents/me/ev/../ceo/x.toml");
    let d = check_caller_scope(&p, &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
}

#[test]
fn link_to_a_peer_dir_is_refused_as_foreign() {
    let h = fixture();
    symlink(h.root.join("agents/ceo"), h.root.join("agents/me/boss")).unwrap();
    let d = check_caller_scope(&h.root.join("agents/me/boss/SOUL.md"), &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedForeignAgentDir { .. }), "{d:?}");
}

#[test]
fn dangling_link_is_refused() {
    // Writing through a dangling link creates its target.
    let h = fixture();
    symlink(h.root.join("new-store.json"), h.root.join("agents/me/out.json")).unwrap();
    let d = check_caller_scope(&h.root.join("agents/me/out.json"), &h.root, &me());
    assert!(!d.is_allowed(), "{d:?}");
}

#[test]
fn not_yet_existing_tail_is_fine() {
    let h = fixture();
    let p = h.root.join("agents/me/new/dir/file.md");
    assert!(check_caller_scope(&p, &h.root, &me()).is_allowed());
}

#[test]
fn home_reached_through_a_link_is_compared_on_its_real_path() {
    let h = fixture();
    let alias = h.root.parent().unwrap().join("home-alias");
    symlink(&h.root, &alias).unwrap();
    // The hook was told the alias; the agent writes the real path.
    let d = check_caller_scope(&h.root.join("tool_calls.jsonl"), &alias, &me());
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
    // …and the other way round.
    let d = check_caller_scope(&alias.join("evals/ceo/x.toml"), &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
    // Own directory through the alias is still own.
    let d = check_caller_scope(&alias.join("agents/me/notes.md"), &h.root, &me());
    assert!(d.is_allowed(), "{d:?}");
}

#[test]
fn own_soul_and_contract_through_a_link_are_refused() {
    let h = fixture();
    symlink(h.root.join("agents/me/SOUL.md"), h.root.join("agents/me/persona.md")).unwrap();
    symlink(h.root.join("agents/me/CONTRACT.toml"), h.root.join("agents/me/rules.toml"))
        .unwrap();
    let d = check_own_soul_write(&h.root.join("agents/me/persona.md"), &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedOwnSoulWrite { .. }), "{d:?}");
    let d = check_own_contract_write(&h.root.join("agents/me/rules.toml"), &h.root, &me());
    assert!(matches!(d, GuardDecision::BlockedOwnContractWrite { .. }), "{d:?}");
}

#[test]
fn operator_is_unaffected_by_link_resolution() {
    let h = fixture();
    symlink(h.root.join("evals/me"), h.root.join("agents/me/ev")).unwrap();
    assert_eq!(
        check_caller_scope(&h.root.join("agents/me/ev/x.toml"), &h.root, &HookCaller::Absent),
        GuardDecision::NotAgentFile
    );
}

#[test]
fn link_loop_is_refused() {
    let h = fixture();
    symlink(h.root.join("agents/me/b"), h.root.join("agents/me/a")).unwrap();
    symlink(h.root.join("agents/me/a"), h.root.join("agents/me/b")).unwrap();
    let d = check_caller_scope(&h.root.join("agents/me/a/x.md"), &h.root, &me());
    assert!(!d.is_allowed(), "{d:?}");
}
