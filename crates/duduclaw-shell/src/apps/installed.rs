// "What is actually installed on this machine" — APP-1 (2026-08-22).
//
// Before this round, `apps::search` filtered `fake_data::DOCK_APPS`: six
// hand-authored entries lifted from a design board, five of which had no
// real app behind them at all. On a real appliance the Launcher and the
// dock therefore showed a menu of things that were not there. This module
// replaces that with a real enumeration of two sources, merged:
//
//   1. **Flatpak** — `flatpak list --app` (`apps/flatpak_list.rs` for the
//      argv/parser and for why it is run twice).
//   2. **Native `.desktop` entries** — the XDG applications directories
//      (`apps/desktop_entry.rs` for the parser and the directory rules).
//
// Both exist on the appliance at the same time (the image ships a native
// chromium AND a flatpak installation at `/data/flatpak`), so neither one
// alone is the answer.
//
// ── Honest failure, never a fallback narrative ──────────────────────────
// Three outcomes are kept strictly apart, because they mean different
// things to the operator and to whoever debugs this later:
//   * `SourceStatus::Absent` — this source does not exist here at all (no
//     `flatpak` binary; no applications directory). NOT an error. A dev Mac
//     is `Absent`/`Absent` and that is the correct, quiet answer.
//   * `SourceStatus::Ok(n)` — enumerated, contributed `n` entries. `Ok(0)`
//     is a real answer too: the source is there and has nothing in it.
//   * `SourceStatus::Failed(reason)` — it IS here and the enumeration
//     failed. The reason is carried (logged by the poll bridge on change,
//     see `home/home_dock.rs`) and the source contributes NOTHING.
// There is no code path anywhere in this module that substitutes canned
// entries for a failed or empty scan. An empty machine renders an empty
// list with an honest message (`apps::feed::AppsEmptyState`), never a
// catalog dressed up as an inventory.
//
// ── Blocking on purpose ─────────────────────────────────────────────────
// `scan()` spawns subprocesses and walks directories. It is called from a
// `std::thread::spawn` and bridged back with `mpsc` + `cx.spawn`, the same
// split `gateway_client` / `comp_client` / `apps::probe_download_size`
// already use — never from a render pass. `apps::feed::InstalledAppsFeed`
// holds the result and decides when a re-scan is due.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::desktop_entry::{self, XdgEnv};
use super::flatpak_list;
use super::icon_resolve::{self, AppIcon, IconMiss};

/// Which enumeration an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AppSource {
    Flatpak,
    Desktop,
}

/// How one source's enumeration went. See this file's header comment for
/// why "not here" and "here but broken" are deliberately different states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceStatus {
    Absent,
    Ok(usize),
    Failed(String),
}

/// One real, installed application.
///
/// `PartialEq` without `Eq` since ICON-2 — see `icon_resolve::AppIcon`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InstalledApp {
    /// Stable identity: the flatpak application id, or the desktop-file id
    /// (`apps/desktop_entry.rs::desktop_file_id`). Also the dedup key
    /// between the two sources — see `merge`.
    pub id: String,
    /// What the operator sees. Locale-resolved for desktop entries.
    pub name: String,
    pub source: AppSource,
    /// The `.desktop` `Icon=` value, verbatim. `None` for a flatpak app
    /// that has no exported `.desktop` file reachable from any applications
    /// directory — `flatpak list` has no icon column, and inventing one
    /// from the app id would be a guess.
    ///
    /// ICON-2 (2026-08-22) turned this from a carried-but-unrendered value
    /// into the input of `resolved_icon` below. It is still kept in its raw
    /// form: it is what the log line names when resolution fails, and it is
    /// the only honest record of what the app actually asked for.
    pub icon: Option<String>,
    /// The real, drawable icon for this app — one ready-to-draw file per
    /// rung of the shell's tile ladder, resolved against the system icon
    /// themes at SCAN time (`apps/icon_resolve.rs`) so that rendering never
    /// touches the filesystem.
    ///
    /// `None` means this app draws the generic application icon, which is
    /// what every desktop OS does in the same situation (research §2.3) —
    /// never a blank tile, and never the app name's first letter, which
    /// APP-1's `glyph()` used to return and which ICON-2 retired: the
    /// research found the monogram tile at no desktop OS at all (it is a
    /// CONTACT-avatar convention). The dock's agent avatars keep their
    /// initials, because those really are people.
    pub resolved_icon: Option<AppIcon>,
    /// Ready-to-spawn argv from a desktop entry's `Exec=`, field codes
    /// already stripped. `None` for a flatpak-sourced app (which launches
    /// through `flatpak run <id>` instead).
    pub exec_argv: Option<Vec<String>>,
    /// `Some` when this app can be launched with `flatpak run <id>`.
    pub flatpak_id: Option<String>,
    /// `Terminal=true` — this app expects to be started inside a terminal
    /// emulator. Recorded, and surfaced only as a diagnostic on launch:
    /// this shell has no terminal to host one in yet, and hiding a class of
    /// genuinely installed apps because of that would be this module
    /// inventing a display policy the spec does not have (`NoDisplay`/
    /// `Hidden` are the spec's hide signals, and both ARE honoured).
    pub terminal: bool,
    /// Lower-cased haystack for `apps::search` — name, id, generic name and
    /// keywords joined. Built once at scan time so search stays a cheap
    /// substring test over already-normalised text.
    pub search_key: String,
    /// Where this came from — a flatpak remote name or the `.desktop` file
    /// path. Diagnostics only (`DUDUCLAW_SHELL_DIAG=1`), never rendered:
    /// internal paths and flags do not belong in operator-facing copy.
    pub origin: String,
}

impl InstalledApp {
    /// The xdg-shell `app_id` this app's windows are expected to carry, for
    /// matching against `home::running_windows::RunningWindowsFeed`. Both
    /// sources converge on the same freedesktop convention: a flatpak app's
    /// app_id is its application id, and a native app's is conventionally
    /// its desktop-file id. A REASONABLE convention, not a guarantee — an
    /// app that sets something else simply never matches, which shows up as
    /// a missing running-indicator dot, never as a wrong one.
    pub fn xdg_app_id(&self) -> &str {
        self.flatpak_id.as_deref().unwrap_or(self.id.as_str())
    }

    /// Whether this shell has a real command for it. Every entry produced
    /// by `scan` has one (a desktop entry with no usable `Exec=` is refused
    /// at parse time, and a flatpak row always has `flatpak run`), so this
    /// is a belt-and-suspenders check for call sites that wire a click.
    pub fn launchable(&self) -> bool {
        self.flatpak_id.is_some() || self.exec_argv.is_some()
    }
}

/// Everything one `scan()` learned, including how each source went.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScanOutcome {
    pub apps: Vec<InstalledApp>,
    pub flatpak: SourceStatus,
    pub desktop: SourceStatus,
}

/// Directory-walk safety rails. A `.desktop` file is a few hundred bytes
/// and applications directories are flat or one level deep in practice;
/// these caps exist so a pathological (or hostile) directory cannot turn a
/// background refresh into an unbounded read.
const MAX_DEPTH: usize = 3;
const MAX_FILES: usize = 2000;
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// BLOCKING. Enumerates both sources and merges them. Never returns an
/// error: a source that is missing or broken is recorded as such and
/// contributes nothing (see this file's header comment).
pub(crate) fn scan() -> ScanOutcome {
    let env = xdg_env_from_process();
    let (flatpak_apps, flatpak) = scan_flatpak();
    let (desktop_apps, desktop) = scan_desktop(&env);
    let mut apps = merge(flatpak_apps, desktop_apps);
    resolve_icons(&mut apps, &env);
    ScanOutcome { apps, flatpak, desktop }
}

/// ICON-2 (2026-08-22). Turns every merged app's `Icon=` into a drawable
/// file, AFTER the merge — a flatpak row only learns its `Icon=` from the
/// exported desktop entry that `merge` folds into it, so resolving earlier
/// would resolve nothing for exactly the apps that need it most.
///
/// The theme chain is built ONCE for the whole list (`index.theme` files
/// are tens of kilobytes and there are dozens of apps), then shared.
///
/// Every miss is logged with its own distinguishable reason, once per app
/// per process (`icons::warn_once`) — a scan runs every 60 seconds forever,
/// and an app that will never have an icon must not produce a log line
/// every minute for the machine's whole uptime. Nothing here can fail the
/// scan: a miss simply leaves `resolved_icon` as `None`, which renders the
/// generic application icon.
fn resolve_icons(apps: &mut [InstalledApp], env: &XdgEnv) {
    if apps.is_empty() {
        return;
    }
    let roots = icon_resolve::icon_roots(env);
    let theme_override = std::env::var(icon_resolve::THEME_ENV).ok();
    let chain = icon_resolve::theme_chain_for(env, &roots, theme_override.as_deref());
    for app in apps.iter_mut() {
        match icon_resolve::resolve(app.icon.as_deref(), &roots, &chain) {
            Ok(icon) => app.resolved_icon = Some(icon),
            Err(miss) => {
                app.resolved_icon = None;
                let message = match miss {
                    IconMiss::NoIconKey => format!(
                        "[app-icon] {}: no Icon= to resolve (a flatpak app with no reachable exported .desktop file, or an entry that declares none) — drawing the generic application icon",
                        app.id
                    ),
                    IconMiss::Unresolved => format!(
                        "[app-icon] {}: Icon={:?} matched no file in any icon theme on this machine — drawing the generic application icon",
                        app.id,
                        app.icon.as_deref().unwrap_or("")
                    ),
                };
                crate::icons::warn_once(&format!("resolve:{}", app.id), &message);
            }
        }
    }
}

/// Reads the XDG-relevant environment once. The only `std::env` reader in
/// the app-enumeration path (together with `resolve_icons`'s one read of
/// the icon-theme override) — everything downstream takes this snapshot as
/// a parameter so it stays testable.
pub(crate) fn xdg_env_from_process() -> XdgEnv {
    XdgEnv {
        home: std::env::var("HOME").ok(),
        data_home: std::env::var("XDG_DATA_HOME").ok(),
        data_dirs: std::env::var("XDG_DATA_DIRS").ok(),
        current_desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
        lang: std::env::var("LC_ALL").ok().or_else(|| std::env::var("LC_MESSAGES").ok()).or_else(|| std::env::var("LANG").ok()),
    }
}

/// Runs `flatpak list` once per installation scope (see
/// `apps/flatpak_list.rs`'s header comment for why two) and merges the rows
/// by application id.
///
/// A missing `flatpak` binary is `Absent`, not `Failed` — that is the
/// ordinary state of a dev Mac and of any appliance build predating A4-2/3,
/// and reporting it as an error would train whoever reads the log to ignore
/// real ones. A non-zero exit from ONE scope while the other succeeds is
/// also not a failure of the whole source: asking for an installation this
/// machine does not have is expected off-appliance.
fn scan_flatpak() -> (Vec<InstalledApp>, SourceStatus) {
    let scopes: [Option<&str>; 2] = [Some(super::FLATPAK_INSTALLATION), None];
    let mut rows: BTreeMap<String, flatpak_list::FlatpakApp> = BTreeMap::new();
    let mut any_ok = false;
    let mut first_failure: Option<String> = None;
    let mut binary_missing = false;

    for scope in scopes {
        let argv = flatpak_list::list_argv(scope);
        match std::process::Command::new("flatpak").args(&argv).output() {
            Ok(output) if output.status.success() => {
                any_ok = true;
                for row in flatpak_list::parse_list(&String::from_utf8_lossy(&output.stdout)) {
                    rows.entry(row.app_id.to_lowercase()).or_insert(row);
                }
            }
            Ok(output) => {
                let reason = format!("flatpak {} exited {:?}: {}", argv.join(" "), output.status.code(), tail(&String::from_utf8_lossy(&output.stderr)));
                if crate::diag_enabled() {
                    eprintln!("[apps] {reason}");
                }
                first_failure.get_or_insert(reason);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                binary_missing = true;
                break;
            }
            Err(e) => {
                let reason = format!("flatpak {} could not run: {e}", argv.join(" "));
                first_failure.get_or_insert(reason);
            }
        }
    }

    if binary_missing {
        if crate::diag_enabled() {
            eprintln!("[apps] no flatpak binary on this machine — flatpak source is absent, not failed");
        }
        return (Vec::new(), SourceStatus::Absent);
    }

    let apps: Vec<InstalledApp> = rows.into_values().map(from_flatpak_row).collect();
    let status = if any_ok {
        SourceStatus::Ok(apps.len())
    } else {
        SourceStatus::Failed(first_failure.unwrap_or_else(|| "flatpak list produced no usable output".to_string()))
    };
    (apps, status)
}

fn from_flatpak_row(row: flatpak_list::FlatpakApp) -> InstalledApp {
    let search_key = search_key_from(&[&row.name, &row.app_id]);
    InstalledApp {
        id: row.app_id.clone(),
        name: row.name,
        source: AppSource::Flatpak,
        // `flatpak list` has no icon column. A merged desktop entry (from
        // flatpak's own exports directory) fills this in with the real
        // `Icon=`; without one it stays honestly unknown.
        icon: None,
        // Resolved after the merge, for the whole list at once — see
        // `resolve_icons`.
        resolved_icon: None,
        exec_argv: None,
        flatpak_id: Some(row.app_id),
        terminal: false,
        search_key,
        origin: row.origin.unwrap_or_else(|| "flatpak".to_string()),
    }
}

/// Walks every XDG applications directory and parses the `.desktop` files
/// it finds. Earlier directories win, exactly as the spec requires: a
/// user's `~/.local/share/applications/foo.desktop` overrides
/// `/usr/share/applications/foo.desktop`.
fn scan_desktop(env: &XdgEnv) -> (Vec<InstalledApp>, SourceStatus) {
    let dirs = desktop_entry::applications_dirs(env);
    let desktops = desktop_entry::current_desktops(env);
    let prefs = desktop_entry::locale_prefs(env.lang.as_deref());
    let path_dirs = path_dirs_from_process();

    let mut by_id: BTreeMap<String, InstalledApp> = BTreeMap::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut dirs_present = 0usize;
    let mut first_failure: Option<String> = None;
    let mut files_seen = 0usize;

    for dir in &dirs {
        if !dir.is_dir() {
            continue;
        }
        dirs_present += 1;
        let mut files: Vec<PathBuf> = Vec::new();
        if let Err(e) = collect_desktop_files(dir, dir, 0, &mut files, &mut files_seen) {
            let reason = format!("could not read {}: {e}", dir.display());
            if crate::diag_enabled() {
                eprintln!("[apps] {reason}");
            }
            first_failure.get_or_insert(reason);
        }
        for file in files {
            let Some(id) = desktop_entry::desktop_file_id(dir, &file) else {
                continue;
            };
            let key = id.to_lowercase();
            // A higher-precedence directory already answered for this id —
            // including with a `NoDisplay`/`Hidden` entry, which the spec
            // says shadows the lower-precedence copy rather than falling
            // through to it.
            if claimed.contains(&key) {
                continue;
            }
            match read_capped(&file) {
                Ok(content) => {
                    claimed.insert(key.clone());
                    let entry = desktop_entry::parse(&content, &prefs);
                    if !desktop_entry::should_display(&entry, &desktops) {
                        continue;
                    }
                    if !try_exec_present(&entry, &path_dirs) {
                        if crate::diag_enabled() {
                            eprintln!("[apps] {} declares TryExec={:?} which is not on this machine — skipped", id, entry.try_exec);
                        }
                        continue;
                    }
                    let Some(exec_argv) = entry.exec.as_deref().and_then(desktop_entry::exec_to_argv) else {
                        continue;
                    };
                    by_id.insert(key, from_desktop_entry(id, entry, exec_argv, &file));
                }
                Err(e) => {
                    let reason = format!("could not read {}: {e}", file.display());
                    if crate::diag_enabled() {
                        eprintln!("[apps] {reason}");
                    }
                    first_failure.get_or_insert(reason);
                }
            }
        }
    }

    let apps: Vec<InstalledApp> = by_id.into_values().collect();
    let status = if dirs_present == 0 {
        SourceStatus::Absent
    } else if apps.is_empty() {
        match first_failure {
            Some(reason) => SourceStatus::Failed(reason),
            None => SourceStatus::Ok(0),
        }
    } else {
        SourceStatus::Ok(apps.len())
    };
    (apps, status)
}

fn from_desktop_entry(id: String, entry: desktop_entry::DesktopEntry, exec_argv: Vec<String>, file: &Path) -> InstalledApp {
    let mut parts: Vec<&str> = vec![entry.name.as_str(), id.as_str()];
    if let Some(generic) = entry.generic_name.as_deref() {
        parts.push(generic);
    }
    for keyword in &entry.keywords {
        parts.push(keyword.as_str());
    }
    let search_key = search_key_from(&parts);
    InstalledApp {
        name: entry.name,
        id,
        source: AppSource::Desktop,
        icon: entry.icon,
        resolved_icon: None,
        exec_argv: Some(exec_argv),
        flatpak_id: None,
        terminal: entry.terminal,
        search_key,
        origin: file.display().to_string(),
    }
}

/// Merges the two sources into one deduplicated, deterministically ordered
/// list.
///
/// **Dedup key** is the id, compared case-insensitively. This is not a
/// heuristic: flatpak exports each installed app's `.desktop` file as
/// `<application-id>.desktop`, so a flatpak app whose exports directory is
/// reachable (which `applications_dirs` makes sure of) legitimately appears
/// in BOTH enumerations under the same id.
///
/// **Flatpak wins the identity and the launch command.** `flatpak run <id>`
/// is the command this crate already ships, already documents, and already
/// has real launch evidence for; the exported desktop file's own `Exec=`
/// merely wraps the same thing with branch/arch flags pinned at export
/// time. **The desktop entry still wins the parts flatpak cannot report**:
/// its `Icon=` (there is no icon column in `flatpak list` at all) and its
/// localized `Name=` when flatpak reported none, plus its keywords for
/// search. Neither side silently overwrites something the other actually
/// knew.
pub(crate) fn merge(flatpak: Vec<InstalledApp>, desktop: Vec<InstalledApp>) -> Vec<InstalledApp> {
    let mut by_key: BTreeMap<String, InstalledApp> = BTreeMap::new();
    for app in flatpak {
        by_key.insert(app.id.to_lowercase(), app);
    }
    for app in desktop {
        match by_key.get_mut(&app.id.to_lowercase()) {
            Some(existing) => absorb_desktop(existing, app),
            None => {
                by_key.insert(app.id.to_lowercase(), app);
            }
        }
    }
    // The invariant every consumer relies on, enforced at the ONE place the
    // final list is produced: a row that reaches the dock or the Launcher
    // is a row this shell can actually launch. Both enumerators already
    // guarantee it (a desktop entry with no usable `Exec=` is refused at
    // parse time, a flatpak row always has `flatpak run`), so this filters
    // nothing today — it is here so that a future source cannot quietly
    // introduce a button that does nothing, which is the exact class of
    // problem this work package exists to remove.
    let mut apps: Vec<InstalledApp> = by_key.into_values().filter(|app| app.launchable()).collect();
    // Deterministic order so the dock does not reshuffle between refreshes.
    // Case-insensitive by display name, id as the tiebreaker.
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    apps
}

fn absorb_desktop(existing: &mut InstalledApp, desktop: InstalledApp) {
    if existing.icon.is_none() {
        existing.icon = desktop.icon;
    }
    if existing.name.trim().is_empty() || existing.name == existing.id {
        // flatpak reported no name (`from_flatpak_row` fell back to the id)
        // — the exported desktop file's localized name is strictly better.
        if !desktop.name.trim().is_empty() {
            existing.name = desktop.name;
        }
    }
    let combined = search_key_from(&[existing.search_key.as_str(), desktop.search_key.as_str()]);
    existing.search_key = combined;
}

/// Builds the lower-cased search haystack. Lower-casing is a no-op for CJK,
/// which is exactly right: a zh-TW app name stays matchable verbatim.
fn search_key_from(parts: &[&str]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for part in parts {
        for token in part.split_whitespace() {
            let token = token.trim().to_lowercase();
            if !token.is_empty() && !seen.contains(&token) {
                seen.push(token);
            }
        }
    }
    seen.join(" ")
}

fn path_dirs_from_process() -> Vec<PathBuf> {
    let Ok(path) = std::env::var("PATH") else {
        return Vec::new();
    };
    path.split(':').filter(|p| !p.trim().is_empty()).map(PathBuf::from).collect()
}

/// The spec's `TryExec` rule: if it names a program that is not present,
/// the entry should not be displayed. An entry with no `TryExec` passes
/// trivially — absence of the hint is not evidence of absence of the app.
fn try_exec_present(entry: &desktop_entry::DesktopEntry, path_dirs: &[PathBuf]) -> bool {
    let Some(try_exec) = entry.try_exec.as_deref() else {
        return true;
    };
    let candidates = desktop_entry::try_exec_candidates(try_exec, path_dirs);
    if candidates.is_empty() {
        return true;
    }
    candidates.iter().any(|c| c.exists())
}

/// Recursive-but-bounded `.desktop` collection. Errors reading a SUBentry
/// are skipped (a dangling symlink in an applications directory is
/// ordinary); an error opening the directory itself propagates so the
/// caller can record it.
fn collect_desktop_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>, files_seen: &mut usize) -> std::io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        if *files_seen >= MAX_FILES {
            if crate::diag_enabled() {
                eprintln!("[apps] stopped after {MAX_FILES} desktop files under {}", root.display());
            }
            return Ok(());
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // `file_type()` does not follow symlinks; `path.is_dir()` does,
        // which is what an applications directory full of symlinked
        // subdirectories needs.
        if file_type.is_dir() || (file_type.is_symlink() && path.is_dir()) {
            collect_desktop_files(root, &path, depth + 1, out, files_seen)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
            *files_seen += 1;
            out.push(path);
        }
    }
    // Deterministic order within a directory regardless of what the
    // filesystem hands back.
    out.sort();
    Ok(())
}

/// Reads a `.desktop` file, refusing anything implausibly large rather than
/// pulling it into memory.
fn read_capped(path: &Path) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} bytes exceeds the {MAX_FILE_BYTES}-byte cap", metadata.len())));
    }
    std::fs::read_to_string(path)
}

/// Last line of a captured stderr, capped — enough to identify a failure in
/// a log without pasting a page of output into it. Character-based, never a
/// raw byte slice (this crate's coding convention 1).
fn tail(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatpak_app(id: &str, name: &str) -> InstalledApp {
        from_flatpak_row(flatpak_list::FlatpakApp { app_id: id.to_string(), name: name.to_string(), origin: Some("flathub".to_string()) })
    }

    fn desktop_app(id: &str, source: &str) -> InstalledApp {
        let entry = desktop_entry::parse(source, &[]);
        let argv = desktop_entry::exec_to_argv(entry.exec.as_deref().unwrap_or("")).expect("test fixtures are launchable");
        from_desktop_entry(id.to_string(), entry, argv, Path::new("/usr/share/applications/x.desktop"))
    }

    #[test]
    fn a_flatpak_row_becomes_a_launchable_entry_with_no_invented_icon() {
        let app = flatpak_app("org.chromium.Chromium", "Chromium");
        assert_eq!(app.source, AppSource::Flatpak);
        assert_eq!(app.flatpak_id.as_deref(), Some("org.chromium.Chromium"));
        assert_eq!(app.icon, None, "flatpak list has no icon column — inventing one from the app id would be a guess");
        assert!(app.launchable());
        assert_eq!(app.xdg_app_id(), "org.chromium.Chromium");
        assert_eq!(app.origin, "flathub");
    }

    #[test]
    fn a_desktop_entry_becomes_a_launchable_entry_that_keeps_its_icon_name() {
        let app = desktop_app("chromium", "[Desktop Entry]\nType=Application\nName=Chromium\nExec=/usr/bin/chromium %U\nIcon=chromium\n");
        assert_eq!(app.source, AppSource::Desktop);
        assert_eq!(app.icon.as_deref(), Some("chromium"), "Icon= must survive for the icon work package");
        assert_eq!(app.exec_argv.as_deref(), Some(["/usr/bin/chromium".to_string()].as_slice()));
        assert_eq!(app.flatpak_id, None);
        assert_eq!(app.xdg_app_id(), "chromium");
    }

    /// ICON-2 retired `InstalledApp::glyph()` — the app name's first
    /// character, drawn in the icon slot. The 2026-08 research sweep
    /// (`research/native-os-2026-08/icon-and-cursor-system-2026-08.md`
    /// §2.3) checked GNOME, KDE, elementary, Windows and Android and found
    /// the monogram tile at NONE of them; all five fall back to a generic
    /// application icon. This test pins the boundary that replaced it: a
    /// freshly-constructed row carries no resolved icon, and the renderer
    /// (`crate::icons::app_icon_element`) turns that into the generic icon
    /// rather than into a letter.
    #[test]
    fn a_row_starts_with_no_resolved_icon_and_no_letter_placeholder() {
        let app = flatpak_app("org.x.Y", "chromium");
        assert_eq!(app.resolved_icon, None);
        assert_eq!(app.icon, None, "the raw Icon= value and the resolved file are separate facts");
    }

    #[test]
    fn search_keys_are_lowercased_deduplicated_and_keep_cjk_verbatim() {
        let app = desktop_app(
            "texteditor",
            "[Desktop Entry]\nType=Application\nName=文字編輯器\nGenericName=Text Editor\nExec=gedit\nKeywords=Text;text;Editor;\n",
        );
        let tokens: Vec<&str> = app.search_key.split_whitespace().collect();
        assert!(tokens.contains(&"文字編輯器"), "a CJK name is lower-cased to itself and stays matchable verbatim");
        assert!(tokens.contains(&"text"));
        assert!(tokens.contains(&"editor"));
        assert_eq!(tokens.iter().filter(|t| **t == "text").count(), 1, "`Text` and `text` must collapse to one token");
    }

    // ── merge / dedup ───────────────────────────────────────────────────

    #[test]
    fn the_same_app_from_both_sources_is_one_row_that_keeps_the_best_of_each() {
        let flatpak = vec![flatpak_app("org.chromium.Chromium", "Chromium")];
        let desktop = vec![desktop_app(
            "org.chromium.Chromium",
            "[Desktop Entry]\nType=Application\nName=Chromium 網頁瀏覽器\nExec=/usr/bin/flatpak run org.chromium.Chromium\nIcon=org.chromium.Chromium\nKeywords=web;\n",
        )];
        let merged = merge(flatpak, desktop);
        assert_eq!(merged.len(), 1, "an app exported by flatpak into an applications dir must not appear twice");
        let app = &merged[0];
        assert_eq!(app.source, AppSource::Flatpak, "the flatpak row owns the identity and the launch command");
        assert_eq!(app.flatpak_id.as_deref(), Some("org.chromium.Chromium"));
        assert_eq!(app.exec_argv, None, "launch goes through `flatpak run`, not the exported Exec line");
        assert_eq!(app.icon.as_deref(), Some("org.chromium.Chromium"), "the desktop entry contributes the icon flatpak cannot report");
        assert_eq!(app.name, "Chromium", "flatpak's own non-empty name is not overwritten");
        assert!(app.search_key.contains("web"), "the desktop entry's keywords still feed search");
    }

    #[test]
    fn dedup_is_case_insensitive_on_the_id() {
        let merged = merge(vec![flatpak_app("Org.Example.App", "A")], vec![desktop_app("org.example.app", "[Desktop Entry]\nType=Application\nName=A\nExec=a\n")]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn a_flatpak_row_with_no_name_adopts_the_desktop_entrys_localized_one() {
        // `from_flatpak_row` falls back to the id when the name column is
        // empty — a real localized name is strictly better than that.
        let merged = merge(
            vec![flatpak_app("org.gnome.TextEditor", "org.gnome.TextEditor")],
            vec![desktop_app("org.gnome.TextEditor", "[Desktop Entry]\nType=Application\nName=文字編輯器\nExec=x\n")],
        );
        assert_eq!(merged[0].name, "文字編輯器");
    }

    #[test]
    fn distinct_apps_from_both_sources_all_survive_in_a_stable_order() {
        let merged = merge(
            vec![flatpak_app("org.chromium.Chromium", "Chromium")],
            vec![
                desktop_app("zed", "[Desktop Entry]\nType=Application\nName=Zed\nExec=zed\n"),
                desktop_app("ark", "[Desktop Entry]\nType=Application\nName=ark\nExec=ark\n"),
            ],
        );
        assert_eq!(merged.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["ark", "Chromium", "Zed"], "sorted case-insensitively by name");
    }

    #[test]
    fn merging_nothing_with_nothing_is_an_empty_list_not_a_fallback_catalog() {
        assert!(merge(Vec::new(), Vec::new()).is_empty());
    }

    // ── source status ───────────────────────────────────────────────────

    #[test]
    fn scan_on_this_machine_never_panics_and_never_invents_entries() {
        // Real call against whatever this machine actually has. On a dev
        // Mac both sources are `Absent` and the list is empty; on Linux CI
        // there may be real apps. Either is a pass — what is asserted is
        // the INVARIANT: every entry that comes out is launchable and
        // carries an id, and an absent source contributes nothing.
        let outcome = scan();
        for app in &outcome.apps {
            assert!(!app.id.is_empty(), "an app with no id could never be deduplicated or focused");
            assert!(!app.name.is_empty(), "an app with no name would render as a blank row");
            assert!(app.launchable(), "scan must never produce a row this shell cannot actually launch");
        }
        if outcome.flatpak == SourceStatus::Absent && outcome.desktop == SourceStatus::Absent {
            assert!(outcome.apps.is_empty(), "no sources means no apps — never a fallback catalog");
        }
    }

    #[test]
    fn a_directory_that_does_not_exist_is_absent_not_failed() {
        let env = XdgEnv {
            home: Some("/duduclaw-nonexistent-home-for-tests".to_string()),
            data_home: Some("/duduclaw-nonexistent-data-for-tests".to_string()),
            data_dirs: Some("/duduclaw-nonexistent-share-for-tests".to_string()),
            ..Default::default()
        };
        let (apps, status) = scan_desktop(&env);
        assert!(apps.is_empty());
        assert_eq!(status, SourceStatus::Absent, "a machine with no applications directory is not a machine with a broken one");
    }

    #[test]
    fn a_real_directory_of_desktop_files_is_read_end_to_end() {
        // The one test that exercises the actual filesystem walk, id
        // precedence, NoDisplay filtering and TryExec skipping together.
        let root = std::env::temp_dir().join(format!("duduclaw-shell-apps-test-{}", std::process::id()));
        let system = root.join("usr/share/applications");
        let user = root.join("home/.local/share/applications");
        std::fs::create_dir_all(system.join("sub")).expect("temp dirs");
        std::fs::create_dir_all(&user).expect("temp dirs");
        std::fs::write(system.join("real.desktop"), "[Desktop Entry]\nType=Application\nName=System Copy\nExec=/bin/true\nIcon=real\n").unwrap();
        std::fs::write(user.join("real.desktop"), "[Desktop Entry]\nType=Application\nName=User Copy\nExec=/bin/true\nIcon=real\n").unwrap();
        std::fs::write(system.join("hidden.desktop"), "[Desktop Entry]\nType=Application\nName=Hidden\nExec=/bin/true\nNoDisplay=true\n").unwrap();
        std::fs::write(system.join("nolink.desktop"), "[Desktop Entry]\nType=Link\nName=Link\nURL=http://x\n").unwrap();
        std::fs::write(
            system.join("missing.desktop"),
            "[Desktop Entry]\nType=Application\nName=Missing\nExec=nope\nTryExec=/duduclaw-nonexistent-binary-for-tests\n",
        )
        .unwrap();
        std::fs::write(system.join("sub/nested.desktop"), "[Desktop Entry]\nType=Application\nName=Nested\nExec=/bin/true\n").unwrap();
        std::fs::write(system.join("notes.txt"), "not a desktop file").unwrap();

        let env = XdgEnv {
            home: Some(root.join("home").display().to_string()),
            data_home: Some(root.join("home/.local/share").display().to_string()),
            data_dirs: Some(root.join("usr/share").display().to_string()),
            ..Default::default()
        };
        let (apps, status) = scan_desktop(&env);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();

        assert!(names.contains(&"User Copy"), "XDG_DATA_HOME must win over XDG_DATA_DIRS");
        assert!(!names.contains(&"System Copy"), "the lower-precedence copy of the same id must be shadowed");
        assert!(!names.contains(&"Hidden"), "NoDisplay=true must not be displayed");
        assert!(!names.contains(&"Link"), "a Link entry is not an application");
        assert!(!names.contains(&"Missing"), "TryExec pointing at a binary that is not here must hide the entry");
        assert!(names.contains(&"Nested"), "subdirectories are walked, with the spec's `dir-file` id rule");
        assert!(apps.iter().any(|a| a.id == "sub-nested"));
        assert!(matches!(status, SourceStatus::Ok(n) if n == apps.len()));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Live-fire, never run by a bare `cargo test` — same `#[ignore]`
    /// contract `oobe::network::nm`'s and `comp_client`'s own live tests
    /// establish. Prints what this machine ACTUALLY reports, which is the
    /// only way to check the two parsers against real data:
    ///
    /// ```text
    /// cargo test -- --ignored live_scan_this_machine --nocapture
    /// ```
    ///
    /// Asserts only the invariants (no panic, every row launchable, statuses
    /// consistent with the rows) — the CONTENT is machine-specific and is
    /// there to be read, not asserted.
    #[test]
    #[ignore]
    fn live_scan_this_machine() {
        let outcome = scan();
        eprintln!("[live] flatpak: {:?}", outcome.flatpak);
        eprintln!("[live] desktop: {:?}", outcome.desktop);
        eprintln!("[live] {} app(s) after merge:", outcome.apps.len());
        for app in &outcome.apps {
            eprintln!(
                "[live]   {:?} id={} name={:?} icon={:?} resolved_icon={:?} xdg_app_id={} exec={:?} terminal={} origin={}",
                app.source,
                app.id,
                app.name,
                app.icon,
                app.resolved_icon,
                app.xdg_app_id(),
                app.exec_argv,
                app.terminal,
                app.origin
            );
        }
        for app in &outcome.apps {
            assert!(app.launchable(), "{} reached the list with no launch command", app.id);
            assert!(!app.name.is_empty());
        }
        if let SourceStatus::Ok(n) = outcome.desktop {
            assert!(n <= outcome.apps.len() + 64, "desktop count {n} is wildly out of line with the merged list");
        }
    }

    /// ICON-2's wiring, end to end on a real directory tree: a `.desktop`
    /// entry whose `Icon=` really is in a real icon theme comes out of
    /// `resolve_icons` with a drawable file for every rung of the ladder,
    /// and one whose `Icon=` answers to nothing comes out honestly empty.
    #[test]
    fn resolve_icons_fills_in_what_the_themes_actually_have() {
        use crate::apps::icon_theme;

        let root = std::env::temp_dir().join(format!("duduclaw-shell-resolve-icons-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let theme_dir = root.join("usr/share/icons/hicolor/48x48/apps");
        std::fs::create_dir_all(&theme_dir).expect("temp dirs");
        std::fs::write(
            root.join("usr/share/icons/hicolor/index.theme"),
            "[Icon Theme]\nDirectories=48x48/apps\n\n[48x48/apps]\nSize=48\nContext=Applications\nType=Fixed\n",
        )
        .unwrap();
        // Header-only PNG: `read_png_header` never decodes pixels.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&48u32.to_be_bytes());
        png.extend_from_slice(&48u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        std::fs::write(theme_dir.join("known.png"), &png).unwrap();

        let mut apps = vec![
            desktop_app("known", "[Desktop Entry]\nType=Application\nName=Known\nExec=/bin/true\nIcon=known\n"),
            desktop_app("unknown", "[Desktop Entry]\nType=Application\nName=Unknown\nExec=/bin/true\nIcon=nothing-answers-to-this\n"),
            desktop_app("naked", "[Desktop Entry]\nType=Application\nName=Naked\nExec=/bin/true\n"),
        ];
        let env = XdgEnv {
            home: Some(root.join("home").display().to_string()),
            data_home: Some(root.join("home/.local/share").display().to_string()),
            data_dirs: Some(root.join("usr/share").display().to_string()),
            ..Default::default()
        };
        resolve_icons(&mut apps, &env);

        let known = apps.iter().find(|a| a.id == "known").expect("present");
        let resolved = known.resolved_icon.as_ref().expect("a real theme icon resolves");
        for container in icon_theme::RENDER_CONTAINERS {
            assert!(resolved.for_container(container).is_some(), "every rung of the ladder must have a file to draw");
        }
        assert_eq!(apps.iter().find(|a| a.id == "unknown").unwrap().resolved_icon, None, "an Icon= nothing answers to must not invent a file");
        assert_eq!(apps.iter().find(|a| a.id == "naked").unwrap().resolved_icon, None, "no Icon= at all is a different miss, same honest result");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stderr_tails_are_capped_and_never_slice_mid_character() {
        assert_eq!(tail("warning: x\nerror: the real reason\n"), "error: the real reason");
        let cjk = "字".repeat(500);
        assert_eq!(tail(&cjk).chars().count(), 200, "a 200-CHARACTER cap, not a byte slice that could split a CJK codepoint");
        assert_eq!(tail(""), "");
    }
}
