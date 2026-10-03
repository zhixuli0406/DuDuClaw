use super::*;

// ── G1 round 2 (M2): the Bash `<home>` rule judges positions, not mentions ──
//
// Round 1 refused any command that had a write-shaped fragment anywhere and
// named a `<home>` path anywhere, which both over-blocked (`ls <home> 2>/dev/null`,
// copying a home file into the agent's own directory) and under-blocked (no
// database CLIs, no `ln`/`chmod`/`rsync`). Each category below has cases that
// must pass and cases that must be refused.

fn me() -> HookCaller {
    agent("sales-rep")
}

fn blocked_home(cmd: &str) {
    let d = check_bash_protected_write(cmd, &home(), &me());
    assert!(
        matches!(d, GuardDecision::BlockedHomeStateWrite { .. }),
        "expected BlockedHomeStateWrite: {cmd}: {d:?}"
    );
}

fn allowed(cmd: &str) {
    let d = check_bash_protected_write(cmd, &home(), &me());
    assert!(d.is_allowed(), "expected allow: {cmd}: {d:?}");
}

#[test]
fn fd_redirects_are_not_writes() {
    for cmd in [
        "ls /Users/alice/.duduclaw/evals 2>/dev/null",
        "ls ~/.duduclaw/evals/sales-rep 2>&1",
        "cd .. && ls 2>&1",
        "cat ../../tool_calls.jsonl >/dev/null",
        "cat ~/.duduclaw/tool_calls.jsonl &>/dev/null",
        "grep x ../../evals/ceo/a.toml 1>&2",
        "wc -l ~/.duduclaw/tool_calls.jsonl 2> /dev/null | tail -1",
    ] {
        allowed(cmd);
    }
}

#[test]
fn output_redirect_counts_only_when_its_target_is_home_state() {
    for cmd in [
        "cat ~/.duduclaw/tool_calls.jsonl > notes.md",
        "cat ../../evals/sales-rep/a.toml > ./copy.toml",
        "grep ok ~/.duduclaw/evals/ceo/a.toml >> /tmp/out.txt",
        "echo hi > ~/.duduclaw/attachments/out.txt",
    ] {
        allowed(cmd);
    }
    for cmd in [
        "echo x > ~/.duduclaw/tool_calls.jsonl",
        "echo x >> ../../evals/sales-rep/held-out/h1.toml",
        "printf x>~/.duduclaw/approvals.json",
        "echo x &> ~/.duduclaw/state.json",
        "echo x 2> ~/.duduclaw/errors.log",
        "cd ~/.duduclaw && echo x > anything",
        "cd ../.. && echo x >> new.txt",
    ] {
        blocked_home(cmd);
    }
}

#[test]
fn copy_commands_are_judged_by_destination() {
    for cmd in [
        "cp ~/.duduclaw/evals/sales-rep/a.toml ./a.toml",
        "cp -r ../../evals/sales-rep ./evals-copy",
        "cp ~/.duduclaw/tool_calls.jsonl /tmp/",
        "install -m 644 ../../shared/wiki/x.md ./x.md",
        "cp report.pdf ~/.duduclaw/attachments/report.pdf",
        "dd if=../../tool_calls.jsonl of=./copy.jsonl",
        "curl -o ./page.html https://example.com",
    ] {
        allowed(cmd);
    }
    for cmd in [
        "cp fake.toml ~/.duduclaw/evals/ceo/a.toml",
        "cp -t ../../evals/sales-rep fake.toml",
        "cp --target-directory=../../evals fake.toml",
        "install fake.toml ~/.duduclaw/skills/x/SKILL.md",
        "dd if=/dev/zero of=../../tool_calls.jsonl",
        "curl -o ~/.duduclaw/skills/x/SKILL.md https://example.com/x",
        "tar -xf payload.tgz -C ~/.duduclaw/evals",
    ] {
        blocked_home(cmd);
    }
}

#[test]
fn mutating_commands_refuse_any_home_argument() {
    for cmd in [
        "mv ~/.duduclaw/tool_calls.jsonl ./x",
        "mv ./x ../../evals/sales-rep/a.toml",
        "rm ~/.duduclaw/evals/ceo/a.toml",
        "rm -rf ~/.duduclaw",
        "rmdir ../../evals/old",
        "unlink ~/.duduclaw/threat_level",
        "ln -s ~/.duduclaw/evals mylink",
        "ln -sf ../../tool_calls.jsonl log",
        "chmod 777 ~/.duduclaw/approvals.json",
        "chown me ../../budget_breaker_state.json",
        "touch ~/.duduclaw/KILLSWITCH.toml",
        "truncate -s 0 tool_calls.jsonl.1",
        "rsync -a ./x/ ~/.duduclaw/skills/",
        "rsync -a ~/.duduclaw/skills/ ./x/ --delete",
        "sed -i '' 's/a/b/' ~/.duduclaw/evals/ceo/a.toml",
        "sed --in-place 's/a/b/' ../../evals/ceo/a.toml",
        "echo x | tee -a ~/.duduclaw/tool_calls.jsonl",
        "find ~/.duduclaw/evals -name '*.toml' -delete",
        "find ../../evals -exec rm {} ;",
        "ls ~/.duduclaw/evals | xargs rm",
        "mkdir ~/.duduclaw/skills/evil",
        "shred ~/.duduclaw/license.json",
        "sudo rm ~/.duduclaw/license.json",
        "env X=1 rm ../../license.json",
    ] {
        blocked_home(cmd);
    }
    // Round 4 (S1): `sed` is not on the read-only list (its `w` command
    // writes files), so even a plain print with a `<home>` file is refused.
    blocked_home("sed -n 1,5p ~/.duduclaw/evals/ceo/a.toml");
    for cmd in [
        "find ~/.duduclaw/evals -name '*.toml'",
        "ls ~/.duduclaw/evals | xargs cat",
        "mv a.md b.md",
        "rm -rf ./build",
        "ln -s ./notes.md ./latest.md",
        "chmod +x ./script.sh",
        "touch notes.md",
        "rsync -a ./src/ /tmp/backup/",
    ] {
        allowed(cmd);
    }
}

#[test]
fn home_databases_are_refused_even_for_reads() {
    for cmd in [
        "sqlite3 ~/.duduclaw/tasks.db 'select * from tasks'",
        "sqlite3 ../../tasks.db .dump",
        "cat ~/.duduclaw/tasks.db-wal",
        "strings ~/.duduclaw/memory.db | head",
        "cd ~/.duduclaw && sqlite3 approvals.db",
        "ls ~/.duduclaw/*.db",
        "sqlite3 /Users/alice/.duduclaw/events.db-shm",
    ] {
        blocked_home(cmd);
    }
    for cmd in [
        "sqlite3 ./mine.db 'select 1'",
        "sqlite3 /tmp/scratch.db",
        "ls ~/.duduclaw",
    ] {
        allowed(cmd);
    }
}

#[test]
fn interpreters_with_a_home_target_stay_refused() {
    for cmd in [
        "python3 -c \"open('/Users/alice/.duduclaw/tool_calls.jsonl','a').write('x')\"",
        "python3 analyze.py ~/.duduclaw/evals/ceo/a.toml",
        "node -e 'require(\"fs\").writeFileSync(\"../../tool_calls.jsonl\",\"\")'",
        "perl -pi -e 's/a/b/' ~/.duduclaw/evals/ceo/a.toml",
        "bash -c 'echo x > ~/.duduclaw/tool_calls.jsonl'",
        "awk '{print > \"../../evals/x.toml\"}' in.txt",
    ] {
        blocked_home(cmd);
    }
    for cmd in ["python3 analyze.py ./data.csv", "node build.js", "bash ./run.sh"] {
        allowed(cmd);
    }
}

#[test]
fn hook_cwd_from_the_envelope_is_honoured() {
    let elsewhere = PathBuf::from("/Users/alice/Project/app");
    // From a project directory `../..` is not `<home>`.
    let d = check_bash_protected_write_in(
        "echo x > ../../tool_calls2.jsonl",
        &home(),
        &me(),
        Some(&elsewhere),
    );
    assert!(d.is_allowed(), "{d:?}");
    // From `<home>` itself a bare name is home state.
    let d = check_bash_protected_write_in("echo x > anything", &home(), &me(), Some(&home()));
    assert!(matches!(d, GuardDecision::BlockedHomeStateWrite { .. }), "{d:?}");
    // A role member running in its employee's directory: relative paths
    // resolve there, not under the member's own scaffold.
    let employee = home().join("agents/writer");
    let d = check_bash_protected_write_in(
        "echo x > notes.md",
        &home(),
        &agent("writer"),
        Some(&employee),
    );
    assert!(d.is_allowed(), "{d:?}");
    let d = check_bash_protected_write_in(
        "echo x > ../ceo/notes.md",
        &home(),
        &agent("writer"),
        Some(&employee),
    );
    assert!(matches!(d, GuardDecision::BlockedForeignAgentDir { .. }), "{d:?}");
}

#[test]
fn untrusted_relative_writes_in_the_cwd_are_refused() {
    // Round 1 gap: with the cwd known to be an agent directory, a bare
    // relative write lands under `agents/`.
    let u = HookCaller::Untrusted("sales-rep".to_string());
    let d = check_bash_protected_write("echo x > notes.md", &home(), &u);
    assert!(matches!(d, GuardDecision::BlockedUntrustedCaller { .. }), "{d:?}");
    let d = check_bash_protected_write("cat notes.md 2>/dev/null", &home(), &u);
    assert!(d.is_allowed(), "{d:?}");
}

#[test]
fn ln_is_a_write_for_the_established_basename_rules() {
    for cmd in ["ln -s SOUL.md alias.md", "ln CONTRACT.toml c.toml", "ln -s agent.toml a.toml"] {
        let d = check_bash_protected_write(cmd, &home(), &me());
        assert!(!d.is_allowed(), "{cmd}: {d:?}");
    }
}

#[test]
fn operator_is_still_unrestricted_on_the_bash_lane() {
    for cmd in [
        "sqlite3 ~/.duduclaw/tasks.db .dump",
        "rm -rf ~/.duduclaw/evals",
        "cd ~/.duduclaw && echo x > anything",
    ] {
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile,
            "{cmd}"
        );
    }
}
