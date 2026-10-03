use super::*;

// ── Round 5 (closing fixes) ─────────────────────────────────────────────

fn me() -> HookCaller {
    agent("sales-rep")
}

fn refused(cmd: &str) {
    let d = check_bash_protected_write(cmd, &home(), &me());
    assert!(!d.is_allowed(), "expected refusal: {cmd:?}");
}

fn allowed(cmd: &str) {
    let d = check_bash_protected_write(cmd, &home(), &me());
    assert!(d.is_allowed(), "expected allow: {cmd:?}: {d:?}");
}

// 1 — `${DUDUCLAW_HOME}` / `${HOME}/.duduclaw` are recognised in argument
// positions exactly like the unbraced spellings.

#[test]
fn braced_home_variables_are_recognised_in_every_position() {
    for cmd in [
        "rm ${DUDUCLAW_HOME}/evals/ceo/a.toml",
        "truncate -s 0 \"${DUDUCLAW_HOME}/tool_calls.jsonl\"",
        "rm '${DUDUCLAW_HOME}/evals/ceo/a.toml'",
        "rm ${HOME}/.duduclaw/evals/ceo/a.toml",
        "rm \"${HOME}/.duduclaw/evals/ceo/a.toml\"",
        "someviewer ${DUDUCLAW_HOME}/evals/ceo/a.toml",
        "someviewer \"${HOME}/.duduclaw/evals/ceo/a.toml\"",
        "echo x > ${DUDUCLAW_HOME}/state.json",
        "echo x > \"${HOME}/.duduclaw/state.json\"",
        "python3 -c \"open('${DUDUCLAW_HOME}/tool_calls.jsonl','a')\"",
        "cp x.toml ${DUDUCLAW_HOME}/evals/ceo/a.toml",
    ] {
        refused(cmd);
    }
    // Same commands without braces, for parity.
    for cmd in [
        "rm $DUDUCLAW_HOME/evals/ceo/a.toml",
        "someviewer $HOME/.duduclaw/evals/ceo/a.toml",
    ] {
        refused(cmd);
    }
    allowed("cat ${DUDUCLAW_HOME}/evals/ceo/a.toml");
    allowed("cp ${DUDUCLAW_HOME}/evals/ceo/a.toml ./a.toml");
}

// 2 — `find`/`fd` action options in `--opt=value` form are actions too.

#[test]
fn search_tool_actions_in_equals_form_are_actions() {
    for cmd in [
        "fd x ~/.duduclaw/evals --exec=rm",
        "fd x ~/.duduclaw/evals --exec-batch=rm",
        "fd x ~/.duduclaw/evals --exec rm",
        "fd x ~/.duduclaw/evals -x rm",
    ] {
        refused(cmd);
    }
    allowed("fd x ~/.duduclaw/evals --type=f");
}

// 3 — a dangling-link refusal on the Bash lane names the real path.

#[cfg(unix)]
#[test]
fn bash_dangling_link_refusal_names_the_full_path() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".duduclaw");
    std::fs::create_dir_all(root.join("agents/me")).unwrap();
    symlink(root.join("new.json"), root.join("agents/me/out.json")).unwrap();
    let cwd = root.join("agents/me");
    let d = check_bash_protected_write_in("echo x > out.json", &root, &agent("me"), Some(&cwd));
    match &d {
        GuardDecision::BlockedHomeStateWrite { attempted_path, .. } => {
            assert!(
                attempted_path.ends_with("agents/me/out.json"),
                "{}",
                attempted_path.display()
            );
        }
        other => panic!("expected BlockedHomeStateWrite, got {other:?}"),
    }
}
