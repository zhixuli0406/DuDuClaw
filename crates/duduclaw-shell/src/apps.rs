// App registry search + launch — WP-A3 (2026-08-22, A-line S5 "殼整合"
// round): the Launcher's typed-search "app" result category and the dock's
// click-to-launch icon both go through this one module rather than each
// re-deriving their own filter/spawn logic.
//
// ── Data source boundary (read this before touching either list) ───────────
// This crate's task brief for this round assumed a live "Flatpak app
// registry" (`flatpak list`, portal-backed metadata) would already exist to
// query. It does not: A2 (`research/native-os-2026-08/flatpak-portal-scope-
// 2026-08.md`) was a container-level INVESTIGATION, not a shipped feature —
// flatpak reached the appliance image only in the SAME wave as this file's
// install support (A4-2/3, 2026-08-22 — `appliance/mkosi.extra/etc/flatpak/
// installations.d/10-duduclaw-data.conf`), and there is still no live
// package-manager query anywhere in this crate. So `search()` below filters
// `fake_data::DOCK_APPS` — a small hand-authored catalog, honestly labeled
// as such in that const's own doc comment — not a live package-manager
// query. Only ONE entry (`browser`, Chromium) carries a real `flatpak_id`
// with actual launch evidence behind it (A2 §3's container PASS); every
// other entry is a conceptual dock icon lifted from the design board with
// no real app behind it at all, and stays `flatpak_id: None`.
//
// ── Why this is a plain fn module, not an `Entity` ──────────────────────────
// Nothing here needs gpui state — `search` is a pure filter over `'static`
// data (same "gpui-free, independently testable" discipline `surface.rs`
// documents for itself) and `launch` is a fire-and-forget `Command::spawn()`
// call with no result to track yet (no "launching…" UI state this round —
// see `launch`'s own doc comment on the honest limitation that implies).

use crate::fake_data::{self, DockApp};

/// Case-insensitive substring search over `fake_data::DOCK_APPS`'
/// `search_key`/`label` fields — backs the Launcher's "app" result category
/// (D12 §0.8 point 3, `commercial/docs/DESIGN-native-gui-gpui-2026-08.md`:
/// NL delegation is the first result class, app/file/system next). An empty
/// query matches every entry — the Launcher's own pre-typing state, same
/// "browse everything" default a fresh search box uses before narrowing.
pub fn search(query: &str) -> Vec<&'static DockApp> {
    let q = query.trim().to_lowercase();
    fake_data::DOCK_APPS.iter().filter(|app| q.is_empty() || app.search_key.contains(&q) || app.label.to_lowercase().contains(&q)).collect()
}

/// Launches a Flatpak-packaged app by application id (`flatpak run <id>`).
/// Fire-and-forget: `Command::spawn()` returns as soon as the child process
/// is forked — it does not wait for the app to actually start, so this never
/// blocks gpui's render thread and needs no background thread hand-off
/// (unlike `gateway_client`'s blocking HTTP calls, which DO need one — see
/// that module's own header comment for why).
///
/// Callers only wire this behind a click handler when `entry.flatpak_id.
/// is_some()` (`overlay/launcher.rs::app_result_row` / `home/home_dock.rs::
/// dock_app`) — matching this crate's established "only wire what's
/// actually real, leave the rest an honest static stub" convention (see
/// `home/home_dock.rs`'s own header comment on Round 3's dock icons). This
/// fn still no-ops safely (a diag line, never a panic) if handed an entry
/// with no `flatpak_id`, so it's safe to call unconditionally too.
///
/// **Known limitation**: on a dev Mac window (no `flatpak` binary at all),
/// or an appliance build predating A4-2/3, this spawn fails with "no such
/// file or directory". That's an expected, logged outcome, not a bug in
/// this fn; this crate has no toast/notification surface to report a launch
/// failure to the user yet (tracked as debt in the WP-A3 report, not
/// silently dropped).
///
/// **No `--installation=data` here, on purpose** — `flatpak run` resolves an
/// app across every configured installation, so the flag would only narrow
/// it and would break launching on any host without a `data` installation.
/// The INSTALL path does carry it, and must: see this file's "Install"
/// section header for what happens if it doesn't.
///
/// **Known argv gap, NOT fixed in this WP** (`appliance/README.md`, measured
/// finding): Flathub's Chromium still selects the X11 ozone backend even
/// with `WAYLAND_DISPLAY` exported, and needs an explicit
/// `--ozone-platform=wayland` that no environment variable can express. The
/// durable fix is a per-app-id argv policy in this fn; it is out of scope
/// for WP-A4-4 (a CPU/retry-storm fix plus the install gate) and is left
/// recorded here rather than silently half-done.
pub fn launch(entry: &DockApp) {
    let Some(app_id) = entry.flatpak_id else {
        if crate::diag_enabled() {
            eprintln!("[apps] launch requested for '{}' but it has no known flatpak_id (honest no-op)", entry.id);
        }
        return;
    };
    match std::process::Command::new("flatpak").arg("run").arg(app_id).spawn() {
        Ok(_child) => {
            if crate::diag_enabled() {
                eprintln!("[apps] launched {app_id} (dock/launcher id={})", entry.id);
            }
        }
        Err(e) => {
            if crate::diag_enabled() {
                eprintln!("[apps] launch failed for {app_id}: {e}");
            }
        }
    }
}

// ── Install (WP-A4-4, 2026-08-22) ───────────────────────────────────────
//
// A3 shipped `launch` (`flatpak run`) and left INSTALL as an open debt.
// Installing changes system state — it downloads and writes hundreds of
// megabytes from a remote the operator may never have looked at — so it
// must never happen silently off one click. The confirmation gate itself
// lives in `overlay::install_gate` (pure state machine) + `overlay::
// launcher` (the sheet); this half is the plumbing it drives.
//
// ── `--installation=data` is MANDATORY, not a preference ────────────────
// The appliance image (A4-2/3, same wave as this WP) ships
// `/etc/flatpak/installations.d/10-duduclaw-data.conf`, which declares an
// ADDITIONAL named installation `data` at `/data/flatpak`. It does NOT move
// the default one. A bare `flatpak install <app>` therefore still writes to
// `/var/lib/flatpak` — on the fixed 5 GB, read-only-by-design root
// partition, which has well under 1.4 GB free, against a measured 2.4 GB
// for Chromium plus the freedesktop runtime. Getting this wrong does not
// degrade, it fills the system partition; and once a repository lands in
// `/var/lib/flatpak`, moving it is surgery (`appliance/README.md`, "The app
// repository lives on `/data`, never on root"). So every argv this module
// builds carries `--installation=data`, the builders are pure functions,
// and there is a test pinning the flag into each one.
//
// `launch` (`flatpak run`) deliberately does NOT carry it: `run` resolves
// across all configured installations, so the flag would only narrow it,
// and adding it would break launching on a dev box that has no `data`
// installation at all. ENUMERATION would need it (`flatpak list` only sees
// the default installation) — this crate has no enumeration path today
// (`search` filters the hardcoded `fake_data::DOCK_APPS` registry, see this
// file's header comment), so there is nothing else to fix here; whoever
// adds a real `flatpak list` later must carry the flag.
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
/// injection guard — it is a "don't let a malformed registry entry turn
/// into an unintended flag" guard, which is the only way a bad value could
/// actually change what runs.
fn is_safe_cli_arg(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

/// The flatpak installation name every command below targets — see this
/// section's header comment for why this is load-bearing rather than a
/// preference. Must stay in sync with the appliance image's own
/// `mkosi.extra/etc/flatpak/installations.d/10-duduclaw-data.conf`.
pub const FLATPAK_INSTALLATION: &str = "data";

/// What the confirmation sheet shows in its "安裝位置" row. Deliberately the
/// STORAGE the operator recognizes (`/data` — the appliance's own data
/// partition, the thing they might one day need to have enough room on),
/// not the internal `--installation=data` flag or the `/data/flatpak`
/// repository layout underneath it. Internal implementation details do not
/// belong in user-facing copy (this project's own §7 communication rule).
pub const INSTALL_DESTINATION_LABEL: &str = "/data";

/// Pure argv builder for the size probe — split out so a test can pin the
/// `--installation=data` flag without a flatpak binary. `None` when either
/// value would be read as a flag (see `is_safe_cli_arg`).
pub fn remote_info_argv(remote: &str, app_id: &str) -> Option<Vec<String>> {
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
pub fn install_argv(remote: &str, app_id: &str) -> Option<Vec<String>> {
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
pub fn parse_download_size(stdout: &str) -> Option<String> {
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
pub fn probe_download_size(remote: &str, app_id: &str) -> Option<String> {
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
/// observe.
///
/// `--assumeyes` is safe here specifically BECAUSE this is only ever
/// reached through the confirmation gate: the human prompt flatpak would
/// otherwise print has already been asked, on screen, with the app name,
/// remote, size and destination shown (`overlay::install_gate`).
pub fn install(remote: &str, app_id: &str) -> Result<(), String> {
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
    use super::*;
    use crate::fake_data::VerifiedTier;

    #[test]
    fn empty_query_returns_every_registry_entry() {
        assert_eq!(search("").len(), fake_data::DOCK_APPS.len());
    }

    #[test]
    fn whitespace_only_query_behaves_like_empty() {
        assert_eq!(search("   ").len(), fake_data::DOCK_APPS.len());
    }

    #[test]
    fn search_matches_the_ascii_search_key_case_insensitively() {
        let results = search("CHROM");
        assert!(results.iter().any(|a| a.id == "browser"), "expected 'CHROM' to find the browser entry");
    }

    #[test]
    fn search_also_matches_the_cjk_label() {
        let results = search("信箱");
        assert!(results.iter().any(|a| a.id == "mail"));
    }

    #[test]
    fn search_with_no_match_returns_empty() {
        assert!(search("zzz_no_such_app_in_the_registry").is_empty());
    }

    #[test]
    fn launch_on_an_entry_with_no_flatpak_id_is_a_safe_noop() {
        let entry = fake_data::DOCK_APPS.iter().find(|a| a.flatpak_id.is_none()).expect("at least one honest display-only stub must exist");
        launch(entry); // must not panic — the only assertion this test needs
    }

    #[test]
    fn launch_on_the_one_real_entry_does_not_panic_even_without_flatpak_installed() {
        // Dev-machine reality per this module's own header comment: `flatpak`
        // is very likely absent, and this MUST still be a clean `Err` path,
        // never a panic — `Command::spawn()` returning `Err` is exactly what
        // "flatpak not installed" looks like, so this is real coverage, not
        // a smoke test with a fake precondition.
        let entry = fake_data::DOCK_APPS.iter().find(|a| a.id == "browser").expect("browser entry must exist");
        assert_eq!(entry.verified, VerifiedTier::Works);
        launch(entry);
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
        // Output shape this module doesn't recognize -> honest blank, never
        // an invented number (task brief: "拿不到就誠實留白，不要編數字").
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
