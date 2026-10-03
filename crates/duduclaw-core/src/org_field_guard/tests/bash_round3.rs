use super::*;

// ── Round 3: regressions of the positional Bash reading against round 1 ──

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

// R2-1 — shell escapes are undone before the command is read.

#[test]
fn line_continuation_does_not_split_a_command() {
    refused("cp fake.toml \\\n  ~/.duduclaw/evals/ceo/a.toml");
    refused("rsync -a ./x/ \\\n ~/.duduclaw/skills/");
    refused("echo x \\\n > ~/.duduclaw/tool_calls.jsonl");
    allowed("cp ~/.duduclaw/evals/ceo/a.toml \\\n  ./a.toml");
}

#[test]
fn backslash_inside_a_command_name_is_still_that_command() {
    refused("r\\m -rf ~/.duduclaw/evals");
    refused("\\rm ~/.duduclaw/license.json");
    refused("c\\p fake.toml ~/.duduclaw/evals/ceo/a.toml");
    refused("ch\\mod 777 ~/.duduclaw/approvals.json");
}

#[test]
fn backslash_inside_a_path_is_still_that_path() {
    refused("echo x > ~/.duduclaw/tool\\_calls.jsonl");
    refused("cp fake.toml ~/.duduclaw/ev\\als/ceo/a.toml");
    // Own SOUL.md through an escaped name.
    let d = check_bash_protected_write("echo x > S\\OUL.md", &home(), &me());
    assert!(matches!(d, GuardDecision::BlockedOwnSoulWrite { .. }), "{d:?}");
}

#[test]
fn an_escaped_redirect_is_an_argument_not_a_write() {
    allowed("echo x \\> ~/.duduclaw/tool_calls.jsonl");
    allowed("echo 'a > ~/.duduclaw/tool_calls.jsonl'");
}

// R2-2 — anything not known to be read-only falls back to the round-1 rule.

#[test]
fn unknown_command_with_a_home_argument_and_a_write_fragment_is_refused() {
    refused("uv run tool ~/.duduclaw/evals/ceo/a.toml > out.txt");
    refused("npx some-tool --out ~/.duduclaw/skills/x/SKILL.md > log.txt");
    refused("vim -c 'w' ~/.duduclaw/evals/ceo/a.toml > /dev/tty");
    refused("git -C ~/.duduclaw/shared/wiki apply patch.diff > log.txt");
}

#[test]
fn prefix_options_with_values_are_skipped_correctly() {
    refused("sudo -u root rm ~/.duduclaw/license.json");
    refused("timeout -s KILL 5 rm ~/.duduclaw/license.json");
    refused("nice -n 10 rm ~/.duduclaw/license.json");
    refused("env -u FOO rm ~/.duduclaw/license.json");
    refused("env -S 'rm ~/.duduclaw/license.json'");
    refused("time -o ~/.duduclaw/state.json ls");
    refused("stdbuf -oL rm ~/.duduclaw/license.json");
}

#[test]
fn known_read_only_commands_still_pass() {
    for cmd in [
        "ls -la ~/.duduclaw/evals",
        "cat ~/.duduclaw/evals/ceo/a.toml",
        "head -n 5 ../../tool_calls.jsonl",
        "grep -rn x ~/.duduclaw/shared/wiki > hits.txt",
        "wc -l ~/.duduclaw/tool_calls.jsonl > count.txt",
        "diff ~/.duduclaw/evals/ceo/a.toml ./a.toml",
        "jq . ~/.duduclaw/channel_status.json > status.json",
        "stat ~/.duduclaw/evals",
        "find ~/.duduclaw/evals -name '*.toml' > list.txt",
        "sudo -u me ls ~/.duduclaw/evals",
    ] {
        allowed(cmd);
    }
}

#[test]
fn unknown_command_without_a_write_fragment_is_refused_since_round_4() {
    // Round 3 let this through (round-1 parity); round 4 (S1) refuses any
    // unrecognised command with a `<home>` argument.
    refused("someviewer ~/.duduclaw/evals/ceo/a.toml");
}

// R2-3 — an unknown working directory makes relative writes strict when the
// command names `<home>` anywhere.

#[test]
fn unknown_cwd_with_a_home_mention_refuses_relative_writes() {
    refused("ls ~/.duduclaw/evals; cd \"$D\" && echo x > a.toml");
    refused("cd $(dirname ~/.duduclaw/evals/x) && cp f.toml a.toml");
    refused("cd - && cat ~/.duduclaw/evals/x; echo y > rel.txt");
    // No `<home>` mention anywhere: the relative write stays unjudged.
    allowed("cd \"$D\" && echo x > a.txt");
}

// R2-5 — the Bash lane resolves links that already exist.

#[cfg(unix)]
#[test]
fn bash_writes_through_an_existing_link_are_judged_on_the_real_path() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".duduclaw");
    std::fs::create_dir_all(root.join("agents/me")).unwrap();
    std::fs::create_dir_all(root.join("evals/me")).unwrap();
    symlink(root.join("evals/me"), root.join("agents/me/ev")).unwrap();
    let cwd = root.join("agents/me");
    for cmd in ["echo x > ev/case.toml", "cd ev && echo x > case.toml", "cp a.toml ev/"] {
        let d = check_bash_protected_write_in(cmd, &root, &agent("me"), Some(&cwd));
        assert!(
            matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
            "{cmd}: {d:?}"
        );
    }
    let d = check_bash_protected_write_in("echo x > notes.md", &root, &agent("me"), Some(&cwd));
    assert!(d.is_allowed(), "{d:?}");
}

// 6 — the own `agent.toml` freeze is an allow-list.

#[test]
fn an_unknown_future_section_is_frozen_by_default() {
    let base = format!("{BASE}\n[future_gate]\nlevel = 1\n");
    let changed = base.replace("level = 1", "level = 9");
    let d = check_protected_toml_write_as(&agent_toml(), &home(), &agent("agnes"), Some(&base), &changed);
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
    // Adding one is a change too.
    let d = check_protected_toml_write_as(
        &agent_toml(),
        &home(),
        &agent("agnes"),
        Some(BASE),
        &format!("{BASE}\n[future_gate]\nlevel = 1\n"),
    );
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
    // A top-level scalar outside any section is frozen as well.
    let d = check_protected_toml_write_as(
        &agent_toml(),
        &home(),
        &agent("agnes"),
        Some(BASE),
        &format!("stray = 1\n{BASE}"),
    );
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
}

// 7 — the dangling-link refusal names the real reason.

#[cfg(unix)]
#[test]
fn dangling_link_refusal_explains_the_link() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".duduclaw");
    std::fs::create_dir_all(root.join("agents/me")).unwrap();
    symlink(root.join("new.json"), root.join("agents/me/out.json")).unwrap();
    let d = check_caller_scope(&root.join("agents/me/out.json"), &root, &agent("me"));
    assert!(!d.is_allowed(), "{d:?}");
    let msg = d.block_message().unwrap();
    assert!(msg.contains("連結"), "{msg}");
    assert!(!msg.contains("委派"), "{msg}");
}
