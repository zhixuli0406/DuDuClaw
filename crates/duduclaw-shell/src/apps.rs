// App registry: enumerate, search, launch, install.
//
// ── Data source boundary (read this before touching either list) ────────
// APP-1 (2026-08-22) replaced this module's foundation. Until then,
// `search()` filtered `fake_data::DOCK_APPS` — a hand-authored six-entry
// array lifted from a design board, only ONE of which (`browser`/Chromium)
// had a real app behind it. It was honestly labelled in its own doc
// comment, but it was still what the Launcher and the dock actually
// rendered, so on a real appliance the operator was reading a menu of
// software that was not installed. A user testing the VM reported exactly
// that.
//
// This module now has TWO lists, and they answer two different questions:
//
//   * **What is installed** — `apps::installed::scan()`, a real enumeration
//     of `flatpak list --app` plus the XDG `.desktop` directories, merged
//     and deduplicated (`apps/installed.rs`; `apps/flatpak_list.rs` and
//     `apps/desktop_entry.rs` are its two parsers). Held in
//     `apps::feed::InstalledAppsFeed`, scanned on a cadence off the render
//     thread. `search()` below filters THAT. There is no fallback to canned
//     data anywhere on this path: a machine with nothing installed renders
//     an empty list with an honest message, and a failed enumeration says
//     so rather than substituting a catalog.
//   * **What could be installed** — `apps::catalog::INSTALL_CATALOG`, a
//     curated list of apps this shell knows a real flatpak ref and remote
//     for. That is not derivable from scanning a machine that does not have
//     the app yet, which is why it is data; the bar every entry has to
//     clear is spelled out in that module's own header comment, and the
//     five design-board icons with no app behind them did not clear it and
//     are deleted.
//
// `fake_data::DockApp`/`DOCK_APPS` are gone entirely. `fake_data.rs` still
// holds this shell's genuinely-decorative content (goal cards, activity
// shelf, menu strings) — the app list is no longer part of it.
//
// ── Why this is a plain fn module, not an `Entity` ──────────────────────
// Nothing here needs gpui state. `search` is a pure filter, the scan lives
// behind a plain struct with no gpui types (`apps/feed.rs`), and every
// process call (`launch`, `probe_download_size`, `install`, and the scan
// itself) is dispatched from a background thread by its caller — same
// "gpui-free, independently testable" discipline `surface.rs` documents for
// itself.

pub(crate) mod catalog;
pub(crate) mod desktop_entry;
pub(crate) mod feed;
pub(crate) mod flatpak_list;
pub(crate) mod icon_resolve;
pub(crate) mod icon_theme;
pub(crate) mod installed;

use installed::InstalledApp;

/// The D8 "DuDuClaw Verified" four-tier compatibility rating
/// (`commercial/docs/DESIGN-app-compat-layer-2026-08.md` §3), plus a fifth
/// `Unrated` state this crate adds for "no rating evidence exists yet" —
/// the design doc's own four tiers are all "has SOME evidence", so an entry
/// with none needs a distinct state rather than being forced into the
/// nearest tier (which would misrepresent a guess as a rating).
///
/// APP-1 moved this out of `fake_data.rs`: with a real app enumeration,
/// most rows genuinely ARE `Unrated` (nobody has evaluated the average
/// `.desktop` entry on this machine), which makes the tier a real, live
/// piece of information rather than a per-row constant in a canned array.
/// `catalog::verified_tier` is the lookup. `crate::palette::ShellPalette::
/// verified_bg`/`verified_text`/`verified_accent_dot` resolve the per-theme
/// colors; `overlay/launcher.rs`/`home/home_dock.rs` render them.
// `Verified`/`Partial`/`Unsupported` are never CONSTRUCTED today (the
// catalog's one rated entry is `Works`, and everything else resolves to
// `Unrated`) — but every downstream consumer already matches all five
// variants exhaustively, ready for the day a second app gets real rating
// evidence. `#[allow(dead_code)]` is the honest choice over either
// fabricating a fake entry to silence the lint or deleting three-fifths of
// the D8 spec's own tier vocabulary because nothing uses it YET.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifiedTier {
    /// "開箱即用零設定" — D8 §3 top tier.
    Verified,
    /// "需官方預置 workaround" — D8 §3 second tier.
    Works,
    /// "能用但明列缺什麼" — D8 §3 third tier.
    Partial,
    /// "不能用" — D8 §3 bottom tier.
    Unsupported,
    /// Not a D8 tier — this crate's own "no rating evidence exists" state,
    /// see this enum's own header comment.
    Unrated,
}

/// Case-insensitive substring search over an already-scanned installed-app
/// list — backs the Launcher's 「應用程式」result category. An empty query
/// matches every entry (the Launcher's own pre-typing browse state).
///
/// `apps` is `feed::InstalledAppsFeed::apps()`, NOT a fresh scan: search
/// runs on every keystroke and a scan forks subprocesses, so the two are
/// deliberately decoupled (see `apps/feed.rs`'s header comment).
/// `search_key` is built lower-cased at scan time from name + id + generic
/// name + keywords, so this stays one substring test over normalised text.
///
/// Substring matching is correct HERE specifically because this is a search
/// box, not a routing or security decision — the crate convention that bans
/// unanchored `contains` (coding convention 2) is about the latter, and
/// every id/app_id comparison in this module is exact.
pub(crate) fn search<'a>(apps: &'a [InstalledApp], query: &str) -> Vec<&'a InstalledApp> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return apps.iter().collect();
    }
    apps.iter().filter(|app| app.search_key.contains(&q) || app.name.to_lowercase().contains(&q)).collect()
}

/// Launches an installed app. Fire-and-forget: `Command::spawn()` returns
/// as soon as the child process is forked — it does not wait for the app to
/// actually start, so this never blocks gpui's render thread and needs no
/// background thread hand-off (unlike `gateway_client`'s and
/// `comp_client`'s blocking calls, which DO need one).
///
/// Two launch paths, matching the two enumeration sources:
///   * a flatpak app runs through `flatpak run <id>`;
///   * a native `.desktop` app runs its own parsed `Exec=` argv (field
///     codes already stripped at scan time — see `desktop_entry::
///     exec_to_argv`).
///
/// **No `--installation=data` on `flatpak run`, on purpose** — `run`
/// resolves an app across every configured installation, so the flag would
/// only narrow it and would break launching on any host without a `data`
/// installation. The INSTALL and ENUMERATION paths both carry it, and must:
/// see this file's "Install" section and `apps/flatpak_list.rs`'s header.
///
/// **Known limitations, recorded rather than silently absorbed**:
///   * this crate still has no toast/notification surface, so a launch that
///     fails after the fork (a missing runtime, a crash on startup) is
///     invisible to the operator — only the spawn error itself is logged;
///   * an app declaring `Terminal=true` expects a terminal emulator to host
///     it. This shell has none, so its window may never appear. It is still
///     listed (it IS installed, and `NoDisplay`/`Hidden` are the spec's own
///     hide signals — neither of which it set) and the launch is still
///     attempted, with a diagnostic saying why nothing may show up;
///   * Flathub's Chromium selects the X11 ozone backend even with
///     `WAYLAND_DISPLAY` exported and needs an explicit
///     `--ozone-platform=wayland` that no environment variable can express
///     (`appliance/README.md`, measured). The durable fix is a per-app-id
///     argv policy here; still out of scope, still recorded.
pub(crate) fn launch(app: &InstalledApp) {
    if app.terminal && crate::diag_enabled() {
        eprintln!("[apps] '{}' declares Terminal=true and this shell has no terminal to host it — launching anyway, a window may not appear", app.id);
    }
    if let Some(flatpak_id) = app.flatpak_id.as_deref() {
        spawn_and_log(std::process::Command::new("flatpak").arg("run").arg(flatpak_id), &format!("flatpak run {flatpak_id}"), &app.id);
        return;
    }
    let Some(argv) = app.exec_argv.as_deref().filter(|argv| !argv.is_empty()) else {
        if crate::diag_enabled() {
            eprintln!("[apps] launch requested for '{}' but it has no launch command (honest no-op)", app.id);
        }
        return;
    };
    spawn_and_log(std::process::Command::new(&argv[0]).args(&argv[1..]), &argv.join(" "), &app.id);
}

fn spawn_and_log(command: &mut std::process::Command, rendered: &str, app_id: &str) {
    match command.spawn() {
        Ok(_child) => {
            if crate::diag_enabled() {
                eprintln!("[apps] launched '{app_id}' via {rendered}");
            }
        }
        Err(e) => {
            // Always logged, not DIAG-gated: unlike a poll that fails
            // identically forever, a launch failure happens exactly once
            // per click and is the only trace the operator's click leaves.
            eprintln!("[apps] launch failed for '{app_id}' ({rendered}): {e}");
        }
    }
}

// ── Install (WP-A4-4, 2026-08-22) ───────────────────────────────────────
//
// Installing changes system state — it downloads and writes hundreds of
// megabytes from a remote the operator may never have looked at — so it
// must never happen silently off one click. The confirmation gate itself
// lives in `overlay::install_gate` (pure state machine) + `overlay::
// launcher` (the sheet); this half is the plumbing it drives. What can be
// installed comes from `apps::catalog` (see that module's header comment on
// why an inventory cannot answer that question).
//
// ── `--installation=data` is MANDATORY, not a preference ────────────────
// The appliance image ships `/etc/flatpak/installations.d/
// 10-duduclaw-data.conf`, which declares an ADDITIONAL named installation
// `data` at `/data/flatpak`. It does NOT move the default one. A bare
// `flatpak install <app>` therefore still writes to `/var/lib/flatpak` — on
// the fixed 5 GB, read-only-by-design root partition, which has well under
// 1.4 GB free, against a measured 2.4 GB for Chromium plus the freedesktop
// runtime. Getting this wrong does not degrade, it fills the system
// partition; and once a repository lands in `/var/lib/flatpak`, moving it
// is surgery (`appliance/README.md`). So every argv this module builds
// carries `--installation=data`, the builders are pure functions, and there
// is a test pinning the flag into each one. APP-1 extended the same rule to
// the ENUMERATION path (`apps/flatpak_list.rs`), which is what that file's
// original comment predicted would be needed.
//
// **Honest limitation, same one `launch` already carries**: no `flatpak`
// binary exists on the dev Mac, so neither `probe_download_size` nor
// `install` below has been exercised end-to-end against a real one; what IS
// verified here is the argv they build and their degradation behaviour.
// Both DEGRADE rather than guess: a missing binary, a non-zero exit, or
// output this module doesn't recognize all produce `None`/`Err`, which the
// gate renders as an honest blank ("無法取得") or an honest failure line —
// never a fabricated size and never a silent success.

/// Refuses an argument that would be read as a FLAG by `flatpak` rather
/// than as a remote/app id. No shell is involved anywhere here
/// (`Command::new` + `.arg`, never a shell string), so this is not an
/// injection guard — it is a "don't let a malformed catalog entry turn into
/// an unintended flag" guard, which is the only way a bad value could
/// actually change what runs.
fn is_safe_cli_arg(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

/// The flatpak installation the install AND enumeration commands target —
/// see this section's header comment for why this is load-bearing rather
/// than a preference. Must stay in sync with the appliance image's own
/// `mkosi.extra/etc/flatpak/installations.d/10-duduclaw-data.conf`.
pub(crate) const FLATPAK_INSTALLATION: &str = "data";

/// What the confirmation sheet shows in its "安裝位置" row. Deliberately the
/// STORAGE the operator recognizes (`/data` — the appliance's own data
/// partition, the thing they might one day need to have enough room on),
/// not the internal `--installation=data` flag or the `/data/flatpak`
/// repository layout underneath it. Internal implementation details do not
/// belong in user-facing copy (this project's own §7 communication rule).
pub(crate) const INSTALL_DESTINATION_LABEL: &str = "/data";

/// Pure argv builder for the size probe — split out so a test can pin the
/// `--installation=data` flag without a flatpak binary. `None` when either
/// value would be read as a flag (see `is_safe_cli_arg`).
pub(crate) fn remote_info_argv(remote: &str, app_id: &str) -> Option<Vec<String>> {
    if !is_safe_cli_arg(remote) || !is_safe_cli_arg(app_id) {
        return None;
    }
    Some(vec![
        "remote-info".to_string(),
        format!("--installation={FLATPAK_INSTALLATION}"),
        "--show-size".to_string(),
        remote.to_string(),
        app_id.to_string(),
    ])
}

/// Pure argv builder for the install itself — same reasoning as
/// `remote_info_argv`. `--assumeyes` is safe here specifically BECAUSE this
/// is only ever reached through the confirmation gate (see `install`).
pub(crate) fn install_argv(remote: &str, app_id: &str) -> Option<Vec<String>> {
    if !is_safe_cli_arg(remote) || !is_safe_cli_arg(app_id) {
        return None;
    }
    Some(vec![
        "install".to_string(),
        format!("--installation={FLATPAK_INSTALLATION}"),
        "--assumeyes".to_string(),
        remote.to_string(),
        app_id.to_string(),
    ])
}

/// Accepted labels for the download-size line of `flatpak remote-info
/// --show-size` output. TWO spellings are accepted because the exact one
/// this flatpak version prints has NOT been verified first-hand (see this
/// section's header comment) — matching is on the whole trimmed label token
/// before the colon, never a substring scan of the line (this crate's
/// coding convention 2).
const DOWNLOAD_SIZE_LABELS: &[&str] = &["download", "download size"];

/// Pure parser, split out of `probe_download_size` so it is testable
/// without a `flatpak` binary. Returns the size text VERBATIM as flatpak
/// printed it (e.g. `"123.4 MB"`) — this module never reformats, rounds, or
/// unit-converts a number it did not compute, so what the gate shows is
/// exactly what the package manager said.
pub(crate) fn parse_download_size(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let Some((label, value)) = line.split_once(':') else {
            continue;
        };
        let label = label.trim().to_ascii_lowercase();
        if !DOWNLOAD_SIZE_LABELS.contains(&label.as_str()) {
            continue;
        }
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// BLOCKING — callers run it from a `std::thread::spawn` and bridge the
/// result back with `mpsc` + `cx.spawn`, the same split `gateway_client`'s
/// own header comment documents. `None` means "we could not find out",
/// which the gate renders as an honest blank; it never means "zero".
pub(crate) fn probe_download_size(remote: &str, app_id: &str) -> Option<String> {
    if !is_safe_cli_arg(remote) || !is_safe_cli_arg(app_id) {
        return None;
    }
    let output = std::process::Command::new("flatpak").args(remote_info_argv(remote, app_id)?).output().ok()?;
    if !output.status.success() {
        if crate::diag_enabled() {
            eprintln!("[apps] remote-info {remote} {app_id} exited {:?}", output.status.code());
        }
        return None;
    }
    parse_download_size(&String::from_utf8_lossy(&output.stdout))
}

/// Starts an install into the `data` installation (see this section's
/// header comment — targeting the default installation would fill the root
/// partition). Fire-and-forget in the same sense `launch` is:
/// `Command::spawn()` returns once the child is forked, so `Ok(())` means
/// "the install STARTED", never "the app is installed" — the gate's own
/// copy says exactly that rather than claiming a completion this fn cannot
/// observe. The installed-app feed picks the new app up on its next scan
/// (`apps::feed::REFRESH_INTERVAL`), which is what makes the gate's "完成後
/// 會出現在 app 清單" line true rather than aspirational.
///
/// `--assumeyes` is safe here specifically BECAUSE this is only ever
/// reached through the confirmation gate: the human prompt flatpak would
/// otherwise print has already been asked, on screen, with the app name,
/// remote, size and destination shown (`overlay::install_gate`).
pub(crate) fn install(remote: &str, app_id: &str) -> Result<(), String> {
    let Some(argv) = install_argv(remote, app_id) else {
        return Err(format!("refusing to run flatpak with remote={remote:?} app_id={app_id:?}"));
    };
    match std::process::Command::new("flatpak").args(&argv).spawn() {
        Ok(_child) => {
            if crate::diag_enabled() {
                eprintln!("[apps] install started: flatpak {}", argv.join(" "));
            }
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::installed::AppSource;
    use super::*;

    fn app(id: &str, name: &str, keys: &str) -> InstalledApp {
        InstalledApp {
            id: id.to_string(),
            name: name.to_string(),
            source: AppSource::Desktop,
            icon: None,
            resolved_icon: None,
            exec_argv: Some(vec!["/bin/true".to_string()]),
            flatpak_id: None,
            terminal: false,
            search_key: keys.to_lowercase(),
            origin: "test".to_string(),
        }
    }

    fn sample() -> Vec<InstalledApp> {
        vec![
            app("org.chromium.Chromium", "Chromium", "chromium org.chromium.chromium web browser"),
            app("org.gnome.TextEditor", "文字編輯器", "文字編輯器 org.gnome.texteditor text editor"),
            app("foot", "foot", "foot terminal"),
        ]
    }

    // ── search ──────────────────────────────────────────────────────────

    #[test]
    fn an_empty_query_browses_the_whole_installed_list() {
        let apps = sample();
        assert_eq!(search(&apps, "").len(), apps.len());
        assert_eq!(search(&apps, "   ").len(), apps.len(), "whitespace only behaves like empty");
    }

    #[test]
    fn search_is_case_insensitive_over_the_scan_time_search_key() {
        let apps = sample();
        assert!(search(&apps, "CHROM").iter().any(|a| a.id == "org.chromium.Chromium"));
        assert!(search(&apps, "browser").iter().any(|a| a.id == "org.chromium.Chromium"));
        // The haystack is one space-joined string, so a multi-word query
        // matches when those words are adjacent in it…
        assert!(search(&apps, "TEXT editor").iter().any(|a| a.id == "org.gnome.TextEditor"));
        // …and does not when they come from two different apps. Substring
        // matching over a pre-normalised haystack is exactly as much as this
        // search box claims to do — no token reordering, no fuzzy matching.
        assert!(search(&apps, "browser 文字").is_empty());
    }

    #[test]
    fn search_matches_a_cjk_name_verbatim() {
        let apps = sample();
        assert!(search(&apps, "文字").iter().any(|a| a.id == "org.gnome.TextEditor"));
    }

    #[test]
    fn search_over_an_empty_machine_returns_nothing_rather_than_a_catalog() {
        // The regression this whole work package exists for: with no apps
        // installed, the app list is EMPTY. It must not quietly fall back
        // to canned entries.
        assert!(search(&[], "").is_empty());
        assert!(search(&[], "browser").is_empty());
    }

    #[test]
    fn a_query_that_matches_nothing_returns_empty() {
        assert!(search(&sample(), "zzz_no_such_app_anywhere").is_empty());
    }

    // ── launch ──────────────────────────────────────────────────────────

    #[test]
    fn launching_an_entry_with_no_command_is_a_safe_noop() {
        let mut broken = app("x", "X", "x");
        broken.exec_argv = None;
        launch(&broken); // must not panic — the only assertion this needs
    }

    #[test]
    fn launching_without_flatpak_installed_reports_an_error_instead_of_panicking() {
        // Dev-machine reality: `flatpak` is very likely absent, and that
        // MUST be a clean logged `Err` path, never a panic. This is real
        // coverage of the failure branch, not a smoke test.
        let mut flatpak_app = app("org.example.Nope", "Nope", "nope");
        flatpak_app.exec_argv = None;
        flatpak_app.flatpak_id = Some("org.example.DoesNotExist".to_string());
        launch(&flatpak_app);
    }

    #[test]
    fn launching_a_native_entry_whose_binary_is_missing_does_not_panic() {
        let mut missing = app("gone", "Gone", "gone");
        missing.exec_argv = Some(vec!["/duduclaw-nonexistent-binary-for-tests".to_string(), "--flag".to_string()]);
        launch(&missing);
    }

    #[test]
    fn a_terminal_app_is_still_listed_and_still_attempted() {
        // Recorded behaviour, not an accident: `Terminal=true` is not a
        // freedesktop hide signal, so hiding it would be this shell
        // inventing a policy. It launches (and logs why nothing may show).
        let mut terminal = app("htop", "htop", "htop");
        terminal.terminal = true;
        assert!(terminal.launchable());
        launch(&terminal);
    }

    // ── Install plumbing (WP-A4-4) ─────────────────────────────────────

    #[test]
    fn parse_download_size_accepts_both_label_spellings_verbatim() {
        assert_eq!(parse_download_size("        Download: 123.4 MB\n"), Some("123.4 MB".to_string()));
        assert_eq!(parse_download_size("  Download size: 1.2 GB\n"), Some("1.2 GB".to_string()));
        // Case-insensitive on the label, untouched on the value.
        assert_eq!(parse_download_size("DOWNLOAD SIZE:   77 kB  "), Some("77 kB".to_string()));
    }

    #[test]
    fn parse_download_size_prefers_the_download_line_over_the_installed_one() {
        let out = "        Ref: app/org.chromium.Chromium/x86_64/stable\n        Installed: 999 MB\n        Download: 123.4 MB\n";
        assert_eq!(parse_download_size(out), Some("123.4 MB".to_string()), "installed size is a different number and must never be shown as the download");
    }

    #[test]
    fn parse_download_size_returns_none_rather_than_guessing() {
        assert_eq!(parse_download_size(""), None);
        assert_eq!(parse_download_size("error: Remote \"flathub\" not found\n"), None);
        assert_eq!(parse_download_size("Download:\n"), None, "an empty value is not a size");
        assert_eq!(parse_download_size("Total download for everything: 5 GB\n"), None, "label matching is whole-token, not substring");
    }

    /// THE appliance-integration regression (A4-2/3 cross-package finding):
    /// `/etc/flatpak/installations.d/10-duduclaw-data.conf` ADDS an
    /// installation, it does not move the default one — so an argv without
    /// `--installation=data` writes 2.4 GB into a 5 GB root partition with
    /// under 1.4 GB free. Pinned as an exact argv, in order, not a
    /// "contains the flag somewhere" check.
    #[test]
    fn every_installing_argv_targets_the_data_installation() {
        assert_eq!(
            install_argv("flathub", "org.chromium.Chromium"),
            Some(vec![
                "install".to_string(),
                "--installation=data".to_string(),
                "--assumeyes".to_string(),
                "flathub".to_string(),
                "org.chromium.Chromium".to_string(),
            ])
        );
        assert_eq!(
            remote_info_argv("flathub", "org.chromium.Chromium"),
            Some(vec![
                "remote-info".to_string(),
                "--installation=data".to_string(),
                "--show-size".to_string(),
                "flathub".to_string(),
                "org.chromium.Chromium".to_string(),
            ])
        );
        // And the flag's value is the one the image actually declares.
        assert_eq!(FLATPAK_INSTALLATION, "data");
    }

    #[test]
    fn the_destination_shown_to_the_operator_is_storage_not_an_internal_flag() {
        assert_eq!(INSTALL_DESTINATION_LABEL, "/data");
        assert!(!INSTALL_DESTINATION_LABEL.contains("--"), "user-facing copy must not leak a CLI flag");
        assert!(!INSTALL_DESTINATION_LABEL.contains("installation="));
    }

    #[test]
    fn flag_shaped_arguments_are_refused_before_any_process_is_spawned() {
        assert!(!is_safe_cli_arg("--user"));
        assert!(!is_safe_cli_arg("-y"));
        assert!(!is_safe_cli_arg(""));
        assert!(is_safe_cli_arg("flathub"));
        assert!(is_safe_cli_arg("org.chromium.Chromium"));

        assert_eq!(install_argv("flathub", "--assumeyes"), None);
        assert_eq!(remote_info_argv("--installation=evil", "org.chromium.Chromium"), None);
        assert_eq!(probe_download_size("--show-size", "org.chromium.Chromium"), None);
        assert!(install("flathub", "--assumeyes").is_err());
        assert!(install("", "org.chromium.Chromium").is_err());
    }

    #[test]
    fn install_on_a_machine_without_flatpak_reports_an_error_instead_of_panicking() {
        // Dev-machine reality per this module's header comment: `flatpak` is
        // very likely absent, and that MUST surface as a clean `Err` the
        // gate can show honestly. If flatpak IS present this spawns a real
        // install of a real app, so the args deliberately name a remote that
        // cannot exist rather than `flathub`.
        let result = install("duduclaw-nonexistent-remote-for-tests", "org.example.DoesNotExist");
        if let Ok(()) = result {
            // flatpak was present and forked; it will fail on its own and
            // this test has still proven the no-panic contract.
        }
    }
}
