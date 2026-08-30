// Windows-VM RemoteApp registry — CP-2 wave-2 (2026-08-30).
//
// The THIRD installed-app source, alongside flatpak (`apps/flatpak_list.rs`)
// and native `.desktop` entries (`apps/desktop_entry.rs`, both merged in
// `apps/installed.rs`): a Windows executable the operator pinned via
// `duduclaw compat windows-vm app-add` on the CLI side
// (`crates/duduclaw-cli/src/compat_windows_vm.rs`), read here and turned
// into an ordinary `InstalledApp` row by `apps::installed::from_windows_vm_entry`
// so the Launcher needs no special-casing at all — it is just another row
// in the same list, launched the same generic way.
//
// ── Fixed path, NOT `$HOME`-relative ─────────────────────────────────────
// This shell runs as the unprivileged `kiosk` OS user (home
// `/data/duduclaw-kiosk`, see `crates/duduclaw-sysd/src/dispatch.rs`'s own
// `KIOSK_HOME_DIR` constant) — a DIFFERENT account from whatever owns
// `/data/duduclaw`, which is where the CLI's own `duduclaw_home()` resolves
// and writes `windows-vm/apps.toml` (see that module's own header comment
// on why the file is deliberately `0644`/`0755`, not the compose file's
// `0600` — this is exactly the cross-user read this crate exists for). So
// [`apps_toml_path`] returns a HARDCODED absolute path, never
// `$HOME`-joined: inside the kiosk session `$HOME` resolves to the kiosk
// user's own home, a directory that will never contain this file.
// [`WINDOWS_VM_APPS_ENV`] overrides it for tests ONLY — same "env override
// so tests don't touch the real filesystem" convention
// `installed::xdg_env_from_process`/`icon_resolve::THEME_ENV` already
// establish; never read on a real appliance any other way.
//
// ── No file = empty list, honestly and silently ──────────────────────────
// Most machines never ran `windows-vm setup` at all, so `windows-vm/` does
// not exist. That is NOT a failure — the same shape `installed::
// scan_flatpak`/`scan_desktop`'s `SourceStatus::Absent` state already
// documents for "this source does not exist here" — so a missing file
// produces an empty list with no log line at all, not a warning repeated
// every 60-second scan forever for the rest of the machine's uptime. This
// source deliberately carries no `SourceStatus` of its own (unlike flatpak/
// desktop): there is no subprocess here that can fail independently of "the
// file is readable or it isn't", so `installed::scan()` folds this
// source's rows straight into the merged list without a third status to
// track.
//
// ── Hand-rolled parser, not the `toml` crate ─────────────────────────────
// This crate deliberately carries no `toml` dependency at all (see
// `Cargo.toml`'s own header comment on why this crate stays detached from
// the root workspace and dependency-light). The CLI side WRITES this file
// with its own hand-rolled emitter too (`compat_windows_vm.rs`'s
// `render_apps_toml`/`toml_basic_string` — NOT `toml::to_string_pretty`,
// whose smallest-escaping heuristic emits single-quoted literal strings
// for backslash-heavy Windows paths, a form this parser deliberately does
// not read; see that file's own "Hand-rolled writer" section for the
// empirical verification), so the shape [`parse_apps_toml`] understands is
// exactly that emitter's output:
// ```text
// [[apps]]
// name = "..."
// exe = "..."
// ```
// repeated blocks, one double-quoted TOML basic string per key. The writer
// only ever needs `\"`/`\\` (Windows exe paths are backslash-heavy, e.g.
// `C:\Program Files\...\winword.exe`) — CR/LF can never appear, the CLI's
// own `sanitize_exe`/`sanitize_display_name` refuse them before anything is
// written — but this parser also defensively unescapes `\n`/`\r`/`\t` for
// the case of a human hand-editing the file. It is NOT a general TOML
// parser: an unrecognized shape is skipped rather than guessed at, matching
// `apps/desktop_entry.rs`'s own "malformed input degrades, never panics,
// never invents a row" discipline.

use std::path::PathBuf;

/// Test-only override — see this file's header comment.
pub(crate) const WINDOWS_VM_APPS_ENV: &str = "DUDUCLAW_SHELL_WINDOWS_VM_APPS";

/// The real, fixed, cross-user path. Never joined with `$HOME` — see this
/// file's header comment on why.
const DEFAULT_APPS_TOML_PATH: &str = "/data/duduclaw/windows-vm/apps.toml";

/// Directory-walk-style safety rail, same spirit as
/// `apps/installed.rs::MAX_FILE_BYTES` scaled down: this is a flat list of
/// short name/exe pairs, not arbitrary `.desktop` files, so it never needs
/// to be anywhere near 256K.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Bounds how many `[[apps]]` blocks one parse produces, regardless of how
/// many the file actually contains — a Launcher with hundreds of Windows
/// tiles is already a degenerate case, and this keeps a pathological (or
/// hand-corrupted) file from turning a 60-second background scan into an
/// unbounded allocation.
const MAX_ENTRIES: usize = 200;

/// One pinned Windows RemoteApp, as read from the registry. Deliberately
/// NOT `InstalledApp` itself — same "parser produces its own small type,
/// `apps/installed.rs` does the conversion" split `flatpak_list::FlatpakApp`
/// and `desktop_entry::DesktopEntry` already establish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsVmApp {
    pub name: String,
    pub exe: String,
}

fn apps_toml_path() -> PathBuf {
    std::env::var_os(WINDOWS_VM_APPS_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_APPS_TOML_PATH))
}

/// BLOCKING (one small file read) — called from `installed::scan`'s own
/// blocking context, the same background thread as the flatpak/`.desktop`
/// scans. Never fails: see this file's header comment on why a missing or
/// unreadable file is an honest empty list, not an error.
pub(crate) fn scan() -> Vec<WindowsVmApp> {
    read_from(&apps_toml_path())
}

fn read_from(path: &std::path::Path) -> Vec<WindowsVmApp> {
    let Ok(metadata) = std::fs::metadata(path) else {
        // No file at all — the ordinary state of a machine that never ran
        // `windows-vm setup`. Honest silence, not a warning (see header
        // comment) — this runs every 60 seconds forever.
        return Vec::new();
    };
    if metadata.len() > MAX_FILE_BYTES {
        if crate::diag_enabled() {
            eprintln!("[apps] {} exceeds the {MAX_FILE_BYTES}-byte cap — refusing to read", path.display());
        }
        return Vec::new();
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        if crate::diag_enabled() {
            eprintln!("[apps] could not read {}", path.display());
        }
        return Vec::new();
    };
    parse_apps_toml(&content)
}

/// Pure parser, split out so it is testable without touching the
/// filesystem. See this file's header comment for the exact shape it
/// understands and why a hand-rolled reader is the right call here.
pub(crate) fn parse_apps_toml(content: &str) -> Vec<WindowsVmApp> {
    let mut apps = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_exe: Option<String> = None;
    let mut in_block = false;

    for line in content.lines() {
        if apps.len() >= MAX_ENTRIES {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[apps]]" {
            if in_block {
                flush_entry(&mut current_name, &mut current_exe, &mut apps);
            }
            in_block = true;
            current_name = None;
            current_exe = None;
            continue;
        }
        if !in_block {
            // A line outside any `[[apps]]` block — not this schema's
            // shape (or a future top-level key this parser does not know
            // about yet). Skipped rather than aborting the whole read.
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let Some(value) = parse_toml_basic_string(value.trim()) else {
            continue;
        };
        match key {
            "name" => current_name = Some(value),
            "exe" => current_exe = Some(value),
            _ => {}
        }
    }
    if in_block {
        flush_entry(&mut current_name, &mut current_exe, &mut apps);
    }
    apps
}

/// Commits one `[[apps]]` block into `out`, if it actually named both
/// fields — a block with only `name` or only `exe` (truncated file, hand
/// edit gone wrong) is dropped rather than guessed into a half-real entry.
fn flush_entry(name: &mut Option<String>, exe: &mut Option<String>, out: &mut Vec<WindowsVmApp>) {
    if let (Some(n), Some(e)) = (name.take(), exe.take()) {
        if !n.is_empty() && !e.is_empty() {
            out.push(WindowsVmApp { name: n, exe: e });
        }
    }
}

/// Un-escapes ONE double-quoted TOML basic string. `None` if `raw` is not a
/// quoted string at all (does not both start AND end with `"`) — a
/// defensive parser never guesses at a malformed line, it skips it.
fn parse_toml_basic_string(raw: &str) -> Option<String> {
    if raw.len() < 2 {
        return None;
    }
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            // An escape this parser does not know (`\uXXXX` and friends,
            // which the writer never actually emits — CJK/emoji app names
            // round-trip as literal UTF-8 in the `toml` crate's own
            // basic-string output, not `\u` escapes) — kept verbatim rather
            // than guessed at.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}

/// The argv `installed::from_windows_vm_entry` puts into `InstalledApp::
/// exec_argv` — reusing `apps::launch`'s ALREADY-generic exec_argv path
/// (`apps.rs::launch`: no flatpak_id, has exec_argv -> `Command::new(argv[0]
/// ).args(&argv[1..])`) rather than adding a new launch branch there. This
/// is genuinely what running it by hand looks like: `duduclaw compat
/// windows-vm app <exe> --name <name>`, backgrounded exactly the same way
/// every other Launcher row already is.
pub(crate) fn launch_argv(entry: &WindowsVmApp) -> Vec<String> {
    vec!["duduclaw".to_string(), "compat".to_string(), "windows-vm".to_string(), "app".to_string(), entry.exe.clone(), "--name".to_string(), entry.name.clone()]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_apps_toml ─────────────────────────────────────────────────

    #[test]
    fn parses_one_block() {
        let apps = parse_apps_toml("[[apps]]\nname = \"Word\"\nexe = \"winword.exe\"\n");
        assert_eq!(apps, vec![WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() }]);
    }

    #[test]
    fn parses_multiple_blocks_in_order() {
        let content = "[[apps]]\nname = \"Word\"\nexe = \"winword.exe\"\n\n[[apps]]\nname = \"Excel\"\nexe = \"excel.exe\"\n";
        let apps = parse_apps_toml(content);
        assert_eq!(apps, vec![
            WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() },
            WindowsVmApp { name: "Excel".to_string(), exe: "excel.exe".to_string() },
        ]);
    }

    #[test]
    fn key_order_within_a_block_does_not_matter() {
        let apps = parse_apps_toml("[[apps]]\nexe = \"a.exe\"\nname = \"A\"\n");
        assert_eq!(apps, vec![WindowsVmApp { name: "A".to_string(), exe: "a.exe".to_string() }]);
    }

    #[test]
    fn unescapes_windows_backslash_paths() {
        let apps = parse_apps_toml("[[apps]]\nname = \"Word\"\nexe = \"C:\\\\Program Files\\\\Office\\\\winword.exe\"\n");
        assert_eq!(apps[0].exe, "C:\\Program Files\\Office\\winword.exe");
    }

    #[test]
    fn unescapes_quotes_and_common_escapes() {
        let apps = parse_apps_toml("[[apps]]\nname = \"Say \\\"Hi\\\"\"\nexe = \"a.exe\"\n");
        assert_eq!(apps[0].name, "Say \"Hi\"");
    }

    #[test]
    fn keeps_cjk_names_verbatim_no_escaping_needed() {
        let apps = parse_apps_toml("[[apps]]\nname = \"記帳軟體\"\nexe = \"ledger.exe\"\n");
        assert_eq!(apps[0].name, "記帳軟體");
    }

    #[test]
    fn empty_content_is_an_empty_list_not_an_error() {
        assert!(parse_apps_toml("").is_empty());
    }

    #[test]
    fn a_block_missing_one_field_is_dropped_not_guessed() {
        let apps = parse_apps_toml("[[apps]]\nname = \"Only Name\"\n");
        assert!(apps.is_empty(), "a half-complete block must not produce a half-real entry");
    }

    #[test]
    fn a_block_with_an_empty_field_is_dropped() {
        let apps = parse_apps_toml("[[apps]]\nname = \"\"\nexe = \"a.exe\"\n");
        assert!(apps.is_empty());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let content = "# a hand-edited registry\n\n[[apps]]\nname = \"Word\"\n# trailing comment\nexe = \"winword.exe\"\n\n";
        assert_eq!(parse_apps_toml(content), vec![WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() }]);
    }

    #[test]
    fn content_with_no_apps_block_at_all_is_empty() {
        assert!(parse_apps_toml("just some\nrandom text\n").is_empty());
    }

    #[test]
    fn unrecognized_keys_are_ignored_without_aborting_the_block() {
        let apps = parse_apps_toml("[[apps]]\nname = \"Word\"\nexe = \"winword.exe\"\nfuture_field = \"x\"\n");
        assert_eq!(apps, vec![WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() }]);
    }

    #[test]
    fn a_line_that_is_not_a_quoted_string_is_skipped_defensively() {
        // Not this parser's shape at all (e.g. a stray inline table or a
        // bare number) — must not panic, must not fabricate a value.
        let apps = parse_apps_toml("[[apps]]\nname = Word\nexe = \"winword.exe\"\n");
        assert!(apps.is_empty(), "an unquoted value must not silently become the entry's name");
    }

    #[test]
    fn parsing_never_produces_more_than_the_entry_cap() {
        let mut content = String::new();
        for i in 0..(MAX_ENTRIES + 50) {
            content.push_str(&format!("[[apps]]\nname = \"App {i}\"\nexe = \"app{i}.exe\"\n"));
        }
        assert_eq!(parse_apps_toml(&content).len(), MAX_ENTRIES);
    }

    // ── read_from (filesystem-backed) ────────────────────────────────────

    #[test]
    fn a_missing_file_is_an_empty_list_not_an_error() {
        let path = std::env::temp_dir().join("duduclaw-shell-windows-vm-apps-missing-for-tests.toml");
        std::fs::remove_file(&path).ok();
        assert!(read_from(&path).is_empty());
    }

    #[test]
    fn a_real_file_is_read_end_to_end() {
        let path = std::env::temp_dir().join(format!("duduclaw-shell-windows-vm-apps-{}.toml", std::process::id()));
        std::fs::write(&path, "[[apps]]\nname = \"Word\"\nexe = \"winword.exe\"\n").unwrap();
        let apps = read_from(&path);
        assert_eq!(apps, vec![WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() }]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_over_the_byte_cap_is_refused_rather_than_read() {
        let path = std::env::temp_dir().join(format!("duduclaw-shell-windows-vm-apps-oversized-{}.toml", std::process::id()));
        let mut content = String::new();
        while (content.len() as u64) <= MAX_FILE_BYTES {
            content.push_str("[[apps]]\nname = \"X\"\nexe = \"x.exe\"\n");
        }
        std::fs::write(&path, &content).unwrap();
        assert!(read_from(&path).is_empty(), "an oversized file must degrade to empty, never a partial parse");
        std::fs::remove_file(&path).ok();
    }

    // ── apps_toml_path() / scan() ───────────────────────────────────────
    //
    // Deliberately NOT tested by mutating `WINDOWS_VM_APPS_ENV` here: that
    // env var is read by `installed::scan()` too (`scan_windows_vm`), which
    // `apps/installed.rs`'s own `scan_on_this_machine_never_panics_and_
    // never_invents_entries`/`live_scan_this_machine` tests call WITHOUT any
    // lock — `cargo test` runs this whole binary's tests concurrently by
    // default, so a `set_var` here (even lock-guarded on THIS module's own
    // side) could still land inside one of those other tests' window and
    // inject a fabricated row into an assertion that specifically checks
    // for an EMPTY list. `apps_toml_path`'s env-override branch is a
    // trivial one-line `var_os(...).map(...).unwrap_or(...)` already
    // exercised by construction every time `read_from`'s own tests run (via
    // its `path` parameter) — no additional coverage is worth that shared
    // risk. See `oobe/claim.rs`'s own header comment for the same
    // "testability split" reasoning applied without this particular
    // cross-file hazard.

    // ── launch_argv ─────────────────────────────────────────────────────

    #[test]
    fn launch_argv_matches_the_hand_run_cli_invocation() {
        let entry = WindowsVmApp { name: "Word".to_string(), exe: "winword.exe".to_string() };
        assert_eq!(
            launch_argv(&entry),
            vec!["duduclaw", "compat", "windows-vm", "app", "winword.exe", "--name", "Word"],
        );
    }
}
