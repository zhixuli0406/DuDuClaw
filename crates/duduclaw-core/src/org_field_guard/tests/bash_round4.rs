use super::*;

// ── Round 4: fd-duplication boundary, strict unknown commands, short
// read-only list ─────────────────────────────────────────────────────────

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

fn strip(s: &str) -> String {
    crate::org_field_guard::bash_parse::strip_fd_redirects(s)
}

// M1 — `>&` followed by digits or `-` is a duplication only at a word
// boundary; `>&1/x` and `>&-/x` write the file `1/x` / `-/x`.

#[test]
fn fd_strip_keeps_a_redirect_whose_word_continues_after_the_digits() {
    for s in [
        "echo x >&1/../../../tool_calls.jsonl",
        "echo x >&-/../../../tool_calls.jsonl",
        "echo x 2>&1/../../../tool_calls.jsonl",
        "cat a >&1agent.toml",
        // the `\` → `/` spelling and the lowercased one go through the same
        // function
        "echo x >&1/../../../tool_calls.jsonl".to_ascii_lowercase().as_str(),
    ] {
        let out = strip(s);
        assert!(out.contains(">&"), "redirect lost: {s:?} -> {out:?}");
    }
}

#[test]
fn fd_strip_still_removes_real_duplications() {
    for (s, gone) in [
        ("ls 2>&1", ">&"),
        ("ls >&2 | wc", ">&"),
        ("ls >&-;echo", ">&"),
        ("(ls >&2)", ">&"),
        ("ls 2>&1>out", "&1"),
        ("ls 1>&2\n", ">&"),
    ] {
        let out = strip(s);
        assert!(!out.contains(gone), "{s:?} -> {out:?}");
    }
    // …and a following real redirect survives.
    assert!(strip("ls 2>&1>out").contains(">out"));
}

#[test]
fn fd_duplication_lookalikes_are_judged_as_writes() {
    // cwd is `<home>/agents/sales-rep`, so `1/../../../` is `<home>`.
    refused("echo x >&1/../../../tool_calls.jsonl");
    refused("echo x >&-/../../../tool_calls.jsonl");
    refused("echo x 2>&1/../../../evals/ceo/a.toml");
    // The text rules see the write fragment again.
    refused("cat a >&1agent.toml");
    // Real duplications are still not writes.
    allowed("ls ~/.duduclaw/evals 2>&1");
    allowed("cat ~/.duduclaw/tool_calls.jsonl >&2");
}

// S1 — an unknown command with a `<home>` argument is refused, write
// fragment or not.

#[test]
fn unknown_command_with_a_home_argument_is_refused_without_a_write_fragment() {
    refused("gzip ~/.duduclaw/tool_calls.jsonl");
    refused("gzip ../../tool_calls.jsonl");
    refused("someviewer ~/.duduclaw/evals/ceo/a.toml");
    refused("xz -z ~/.duduclaw/evals/ceo/a.toml");
    allowed("gzip ./notes.md");
    allowed("someviewer ./notes.md");
}

// S2 — commands that can write or run other programs left the read-only
// list.

#[test]
fn removed_read_only_entries_are_judged_strictly() {
    for cmd in [
        "tree -o ~/.duduclaw/x.txt ~/.duduclaw",
        "rg --pre ./evil.sh x ~/.duduclaw/shared/wiki",
        "less ~/.duduclaw/tool_calls.jsonl",
        "more ~/.duduclaw/tool_calls.jsonl",
        "bat --pager './x' ~/.duduclaw/tool_calls.jsonl",
        "file -C -m ~/.duduclaw/magic",
        "ag --pager ./x y ~/.duduclaw/shared",
        "ack --pager=./x y ~/.duduclaw/shared",
        "sort --compress-program=./evil ~/.duduclaw/tool_calls.jsonl",
        "ll ~/.duduclaw",
    ] {
        refused(cmd);
    }
    for cmd in [
        "ls -la ~/.duduclaw/evals",
        "cat ~/.duduclaw/tool_calls.jsonl | head",
        "grep -rn x ~/.duduclaw/shared/wiki > hits.txt",
        "head -n 5 ../../tool_calls.jsonl",
        "find ~/.duduclaw/evals -name '*.toml' > list.txt",
        "rg x ./src",
        "less ./notes.md",
    ] {
        allowed(cmd);
    }
}
