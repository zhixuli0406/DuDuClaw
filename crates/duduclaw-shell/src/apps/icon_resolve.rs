// Icon Theme Specification — the I/O half. ICON-2 (2026-08-22).
//
// `apps/icon_theme.rs` holds every rule; this file is the part that touches
// the filesystem: where to look, which themes to walk, which files exist,
// and reading the handful of PNG headers the full-bleed judgement needs.
// Same split `apps/desktop_entry.rs` (pure) / `apps/installed.rs` (I/O)
// already uses.
//
// ── BLOCKING, and only ever called from the scan thread ─────────────────
// `resolve` runs inside `apps::installed::scan()`, which
// `home/home_dock.rs::trigger_installed_apps_refresh_if_stale` dispatches
// on a `std::thread::spawn`. Nothing here may be reached from a render
// pass — which is exactly why the result is a fully-resolved `AppIcon`
// carrying one ready-to-draw variant per rung of the ladder
// (`icon_theme::RENDER_CONTAINERS`) rather than something the renderer has
// to look up.
//
// ── Cost ────────────────────────────────────────────────────────────────
// Each `index.theme` is parsed ONCE per scan (`ThemeChain::load`), not once
// per app — Adwaita's is ~30 KB and there are dozens of apps. Within a
// theme only the directories that can hold an application icon are probed
// (`icon_theme::serves_apps`), and only two extensions are tried, so a
// typical app costs a few dozen `stat` calls; PNG headers are read for at
// most one file per rung of the ladder (two), never for every candidate.
//
// ── Honest failure ──────────────────────────────────────────────────────
// There are three ways an app can end up with no icon, they mean different
// things, and `IconMiss` keeps them apart so the log line does too
// (`apps/installed.rs` is what prints it, once per app). None of them
// degrade into drawing nothing: `crate::icons::app_icon_element` falls back
// to the generic application icon, which is what every desktop OS does
// (research §2.3).

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::desktop_entry::XdgEnv;
use super::icon_theme::{self, IconCandidate, ThemeIndex};

/// Where the shell looks for icons.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IconRoots {
    /// Base directories that contain THEME directories (`<root>/<theme>/…`).
    pub themed: Vec<PathBuf>,
    /// The spec's unthemed fallback directories, searched last and flat.
    pub unthemed: Vec<PathBuf>,
}

/// A resolved icon: one ready-to-draw file per rung of the shell's tile
/// ladder. Held on `apps::installed::InstalledApp`.
///
/// `PartialEq` but not `Eq`: `draw_px` is a logical-pixel length, and the
/// only comparison anyone performs on it is `apps::feed::apply_scan`'s
/// "did anything the UI draws actually move" check, which compares two
/// identically-computed values. That propagates the missing `Eq` to
/// `InstalledApp`/`ScanOutcome`, where nothing needed it either.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppIcon {
    variants: Vec<AppIconVariant>,
}

/// What to draw in ONE container size.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppIconVariant {
    /// `icon_theme::container_key` of the tile this was resolved for.
    pub container_key: u32,
    pub path: PathBuf,
    /// Side length to draw the image at, in logical px. Already respects
    /// "never upscale" (`icon_theme::draw_px`).
    pub draw_px: f32,
    /// The image is provably a square opaque raster, so it may be clipped
    /// to the tile's own corner radius and drawn edge to edge. See
    /// `icon_theme::is_full_bleed_square` for why this is proof-only.
    pub full_bleed: bool,
}

impl AppIcon {
    pub(crate) fn for_container(&self, container_px: f32) -> Option<&AppIconVariant> {
        let key = icon_theme::container_key(container_px);
        self.variants.iter().find(|v| v.container_key == key)
    }
}

/// Why an app has no icon to draw. Three distinct facts, three distinct log
/// lines — the same discipline `apps::installed::SourceStatus` applies to a
/// whole enumeration, applied here to one app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconMiss {
    /// The `.desktop` entry has no `Icon=` key at all (or the app came from
    /// `flatpak list`, which has no icon column and no reachable exported
    /// desktop file). Nothing was looked up, because there was nothing to
    /// look up.
    NoIconKey,
    /// There IS an `Icon=` value and nothing in any theme, or on disk,
    /// answers to it.
    Unresolved,
}

// ── Search roots ────────────────────────────────────────────────────────

/// The icon-theme base directories, highest precedence first.
///
/// Spec part: `$HOME/.icons` (legacy but still honoured), then
/// `$XDG_DATA_HOME/icons`, then each `$XDG_DATA_DIRS` entry with `/icons`
/// appended, with `/usr/share/pixmaps` as the UNTHEMED fallback.
///
/// Then, at lower precedence, flatpak's own export directories — for
/// exactly the reason `desktop_entry::applications_dirs` already documents
/// for its own list: flatpak publishes each app's icons into
/// `<installation>/exports/share/icons`, and it is flatpak's `profile.d`
/// snippet, not the base spec, that puts those on `XDG_DATA_DIRS`. A kiosk
/// session that never sourced that snippet would otherwise see every
/// flatpak app fall back to the generic icon. The appliance's own extra
/// installation (`/data/flatpak`) is listed first among them, same as
/// there. Duplicates are removed, so a session that DID source the snippet
/// is unaffected.
pub(crate) fn icon_roots(env: &XdgEnv) -> IconRoots {
    let mut themed: Vec<PathBuf> = Vec::new();
    let home = env.home.as_deref().map(str::trim).filter(|v| !v.is_empty());
    if let Some(home) = home {
        themed.push(PathBuf::from(home).join(".icons"));
    }
    let data_home = resolved_data_home(env);
    if let Some(data_home) = &data_home {
        themed.push(PathBuf::from(data_home).join("icons"));
    }
    let data_dirs = env.data_dirs.as_deref().filter(|v| !v.trim().is_empty()).unwrap_or("/usr/local/share:/usr/share");
    for part in data_dirs.split(':') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        themed.push(PathBuf::from(part).join("icons"));
    }
    themed.push(PathBuf::from("/data/flatpak/exports/share/icons"));
    themed.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    if let Some(data_home) = &data_home {
        themed.push(PathBuf::from(data_home).join("flatpak/exports/share/icons"));
    }

    IconRoots { themed: dedup(themed), unthemed: vec![PathBuf::from("/usr/share/pixmaps")] }
}

fn resolved_data_home(env: &XdgEnv) -> Option<String> {
    if let Some(explicit) = env.data_home.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(explicit.to_string());
    }
    let home = env.home.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
    Some(format!("{home}/.local/share"))
}

fn dedup(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen
}

// ── Which theme ─────────────────────────────────────────────────────────

/// The spec's own universal fallback theme: "there should always exist a
/// theme called 'hicolor'".
const HICOLOR: &str = "hicolor";

/// The theme assumed when nothing says otherwise. Adwaita is what the
/// appliance image actually ships (verified on the VM) and what every
/// GTK-based distribution defaults to; `hicolor` is still searched after
/// it, so a machine without Adwaita loses nothing.
const DEFAULT_THEME: &str = "Adwaita";

/// Env override for the icon theme, in the same `DUDUCLAW_SHELL_*` family
/// as `DUDUCLAW_SHELL_DEBUG_SURFACE` / `DUDUCLAW_SHELL_SKIP_OOBE`.
pub(crate) const THEME_ENV: &str = "DUDUCLAW_SHELL_ICON_THEME";

/// Which icon theme to search first.
///
/// This shell has no settings daemon of its own, so there is no "our"
/// setting to read. In precedence order: the explicit env override, then
/// the user's GTK preference (`~/.config/gtk-3.0/settings.ini`'s
/// `gtk-icon-theme-name`, which is what actually decides this on a normal
/// desktop), then `Adwaita`. `env_override` and `gtk_settings_ini` are
/// parameters, not reads, so this stays testable.
pub(crate) fn preferred_theme(env_override: Option<&str>, gtk_settings_ini: Option<&str>) -> String {
    if let Some(name) = env_override.map(str::trim).filter(|v| icon_theme::is_safe_theme_name(v)) {
        return name.to_string();
    }
    if let Some(name) = gtk_settings_ini.and_then(icon_theme::gtk_icon_theme_name) {
        return name;
    }
    DEFAULT_THEME.to_string()
}

/// The ordered theme search chain: the preferred theme, its `Inherits`
/// parents depth-first, then `hicolor` — the spec's `FindIcon` /
/// `FindIconHelper` order.
#[derive(Debug, Clone, Default)]
pub(crate) struct ThemeChain {
    themes: Vec<LoadedTheme>,
}

#[derive(Debug, Clone)]
struct LoadedTheme {
    name: String,
    index: ThemeIndex,
    /// The subset of `IconRoots::themed` that actually has a `<root>/<name>`
    /// directory, resolved once per scan. Without this, every icon lookup
    /// would probe every theme directory under all seven roots, six of
    /// which typically do not host that theme at all — multiplying the stat
    /// count by ~7 for nothing.
    roots: Vec<PathBuf>,
}

/// Cap on how many themes one chain may visit. A cycle in `Inherits` is
/// already impossible (visited names are tracked), so this only guards
/// against a pathologically deep hand-written chain.
const MAX_THEMES: usize = 16;
/// `index.theme` files are a few tens of KB; anything larger is not one.
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
/// How many leading bytes of a PNG to read for its header. `png_info` needs
/// the 33-byte prefix plus whatever ancillary chunks precede `IDAT`; 8 KB
/// covers a `tRNS` palette comfortably without pulling in pixel data.
const PNG_HEADER_BYTES: usize = 8 * 1024;

impl ThemeChain {
    /// BLOCKING. Reads and parses one `index.theme` per theme in the chain.
    /// Called ONCE per scan; see this file's header comment on cost.
    pub(crate) fn load(roots: &IconRoots, preferred: &str) -> Self {
        let mut themes: Vec<LoadedTheme> = Vec::new();
        let mut pending: VecDeque<String> = VecDeque::new();
        if icon_theme::is_safe_theme_name(preferred) {
            pending.push_back(preferred.to_string());
        }
        pending.push_back(HICOLOR.to_string());

        while let Some(name) = pending.pop_front() {
            if themes.len() >= MAX_THEMES {
                break;
            }
            if themes.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
                continue;
            }
            let index = read_theme_index(roots, &name);
            let present: Vec<PathBuf> = roots.themed.iter().filter(|root| root.join(&name).is_dir()).cloned().collect();
            // Parents are searched right after the theme itself (the spec's
            // depth-first `FindIconHelper`), so they go to the FRONT of the
            // queue — ahead of `hicolor`, which must stay last.
            for parent in index.inherits.iter().rev() {
                if icon_theme::is_safe_theme_name(parent) {
                    pending.push_front(parent.clone());
                }
            }
            themes.push(LoadedTheme { name, index, roots: present });
        }
        // A theme root that has no `index.theme` at all still contributes
        // nothing rather than being dropped from the chain — its entry is
        // kept with an empty index so `hicolor`'s own presence is never
        // conditional on a readable Adwaita.
        Self { themes }
    }

    #[cfg(test)]
    fn names(&self) -> Vec<&str> {
        self.themes.iter().map(|t| t.name.as_str()).collect()
    }
}

/// Reads a theme's `index.theme` from the FIRST root that has one. The spec
/// allows a theme to be spread across several base directories; the index
/// is taken from the highest-precedence copy, while icon FILES are looked
/// up in every root (see `collect_candidates`).
fn read_theme_index(roots: &IconRoots, theme: &str) -> ThemeIndex {
    for root in &roots.themed {
        let path = root.join(theme).join("index.theme");
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_INDEX_BYTES {
            if crate::diag_enabled() {
                eprintln!("[app-icon] {} is {} bytes — refusing to parse it as an index.theme", path.display(), metadata.len());
            }
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => return icon_theme::parse_index_theme(&content),
            Err(e) => {
                if crate::diag_enabled() {
                    eprintln!("[app-icon] could not read {}: {e}", path.display());
                }
            }
        }
    }
    ThemeIndex::default()
}

// ── Resolution ──────────────────────────────────────────────────────────

/// Resolves ONE app's `Icon=` value into a drawable icon, or the reason it
/// could not be.
pub(crate) fn resolve(icon_value: Option<&str>, roots: &IconRoots, chain: &ThemeChain) -> Result<AppIcon, IconMiss> {
    let Some(value) = icon_value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Err(IconMiss::NoIconKey);
    };

    // The spec is explicit: "If the name is an absolute path, the given
    // file will be used." No theme lookup, no extension guessing.
    let candidates = if Path::new(value).is_absolute() { absolute_candidate(value) } else { collect_candidates(value, roots, chain) };
    if candidates.is_empty() {
        return Err(IconMiss::Unresolved);
    }

    let mut variants: Vec<AppIconVariant> = Vec::new();
    for container_px in icon_theme::RENDER_CONTAINERS {
        let target_px = icon_theme::content_px(container_px);
        let Some(chosen) = icon_theme::pick(&candidates, target_px.round().max(0.) as u32) else {
            continue;
        };
        let header = read_png_header(&chosen.path);
        variants.push(AppIconVariant {
            container_key: icon_theme::container_key(container_px),
            path: chosen.path.clone(),
            draw_px: icon_theme::draw_px(target_px, chosen, header.map(|h| h.width)),
            full_bleed: icon_theme::is_full_bleed_square(chosen.kind, header.as_ref()),
        });
    }
    if variants.is_empty() {
        return Err(IconMiss::Unresolved);
    }
    Ok(AppIcon { variants })
}

/// BLOCKING. Builds the theme chain once for a whole scan.
///
/// `env_override` is passed in rather than read here so this module stays
/// free of `std::env` — `apps::installed::xdg_env_from_process` remains the
/// single environment reader on the app-enumeration path.
pub(crate) fn theme_chain_for(env: &XdgEnv, roots: &IconRoots, env_override: Option<&str>) -> ThemeChain {
    let gtk_ini = read_gtk_settings(env);
    ThemeChain::load(roots, &preferred_theme(env_override, gtk_ini.as_deref()))
}

/// `~/.config/gtk-3.0/settings.ini`, if it exists. `XDG_CONFIG_HOME` is
/// deliberately not consulted: `XdgEnv` does not carry it (the desktop-entry
/// rules never needed it), and inventing a second, partly-populated
/// environment snapshot here would be worse than defaulting to `$HOME/
/// .config`, which is what the variable defaults to anyway.
fn read_gtk_settings(env: &XdgEnv) -> Option<String> {
    let home = env.home.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
    for version in ["gtk-4.0", "gtk-3.0"] {
        let path = PathBuf::from(home).join(".config").join(version).join("settings.ini");
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_INDEX_BYTES {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if icon_theme::gtk_icon_theme_name(&content).is_some() {
                return Some(content);
            }
        }
    }
    None
}

fn absolute_candidate(value: &str) -> Vec<IconCandidate> {
    let path = PathBuf::from(value);
    let Some(kind) = path.extension().and_then(|e| e.to_str()).and_then(icon_theme::kind_for_extension) else {
        // A path this shell cannot draw (`.xpm`, no extension at all) is a
        // miss, not a silent blank tile — see `icon_theme::ICON_EXTENSIONS`.
        return Vec::new();
    };
    if !path.is_file() {
        return Vec::new();
    }
    // `DirSizing::UNKNOWN` — an absolute path carries no theme size. `pick`
    // treats that as smaller than any request, which is right: it is chosen
    // because it is the only candidate, and `draw_px` then falls back to
    // the file's own header for the real size.
    vec![IconCandidate { path, kind, sizing: icon_theme::DirSizing::UNKNOWN }]
}

/// The spec's `FindIcon`, collecting every size instead of stopping at one.
///
/// Per-theme, not per-directory: "As soon as there is an icon of any size
/// that matches in a theme, the search is stopped" — so a theme that
/// answers at all ends the search, and its parents (and `hicolor`) are only
/// consulted when it answers with nothing. Within a theme every matching
/// directory is collected, because `icon_theme::pick` needs the whole size
/// ladder to choose "nearest but not smaller" and to prefer a scalable SVG.
///
/// Two phases per theme, for cost (see `icon_theme`'s header comment): the
/// likely application directories first, then — only if those answered with
/// nothing — everything else the theme declares. The second phase is what
/// makes the first one a hint rather than a blind spot.
fn collect_candidates(name: &str, roots: &IconRoots, chain: &ThemeChain) -> Vec<IconCandidate> {
    let Some(icon_name) = icon_theme::theme_icon_name(name) else {
        return Vec::new();
    };
    for theme in &chain.themes {
        let (likely, rest): (Vec<&icon_theme::ThemeDir>, Vec<&icon_theme::ThemeDir>) =
            theme.index.dirs.iter().partition(|d| icon_theme::serves_apps(d));
        for phase in [likely, rest] {
            let found = probe_dirs(&icon_name, &theme.roots, &theme.name, &phase);
            if !found.is_empty() {
                return found;
            }
        }
    }

    // The spec's `LookupFallbackIcon`: the unthemed directories, flat.
    let mut fallback: Vec<IconCandidate> = Vec::new();
    for root in &roots.unthemed {
        for extension in icon_theme::ICON_EXTENSIONS {
            let path = root.join(format!("{icon_name}.{extension}"));
            if !path.is_file() {
                continue;
            }
            let Some(kind) = icon_theme::kind_for_extension(extension) else {
                continue;
            };
            fallback.push(IconCandidate { path, kind, sizing: icon_theme::DirSizing::UNKNOWN });
        }
    }
    fallback
}

/// Probes one theme's directories for `<icon_name>.<ext>`, in the order the
/// index declared them and with `ICON_EXTENSIONS` (SVG first) inside that —
/// a deterministic order, which is what makes `icon_theme::pick`'s
/// tie-breaks reproducible on the same machine.
fn probe_dirs(icon_name: &str, roots: &[PathBuf], theme: &str, dirs: &[&icon_theme::ThemeDir]) -> Vec<IconCandidate> {
    let mut found: Vec<IconCandidate> = Vec::new();
    for root in roots {
        let theme_root = root.join(theme);
        for dir in dirs {
            for extension in icon_theme::ICON_EXTENSIONS {
                let path = theme_root.join(&dir.path).join(format!("{icon_name}.{extension}"));
                if !path.is_file() {
                    continue;
                }
                let Some(kind) = icon_theme::kind_for_extension(extension) else {
                    continue;
                };
                found.push(IconCandidate { path, kind, sizing: dir.sizing });
            }
        }
    }
    found
}

/// Reads a PNG's leading bytes and parses its header. `None` for an SVG, an
/// unreadable file, or anything that is not a PNG — every one of which
/// `icon_theme::is_full_bleed_square` treats as "cannot prove it".
fn read_png_header(path: &Path) -> Option<icon_theme::PngInfo> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; PNG_HEADER_BYTES];
    let mut filled = 0usize;
    loop {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if filled >= buffer.len() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    buffer.truncate(filled);
    icon_theme::png_info(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── search roots ────────────────────────────────────────────────────

    #[test]
    fn icon_roots_follow_xdg_precedence_and_append_the_flatpak_exports() {
        let env = XdgEnv { home: Some("/home/op".to_string()), ..Default::default() };
        let roots = icon_roots(&env);
        assert_eq!(
            roots.themed,
            vec![
                PathBuf::from("/home/op/.icons"),
                PathBuf::from("/home/op/.local/share/icons"),
                PathBuf::from("/usr/local/share/icons"),
                PathBuf::from("/usr/share/icons"),
                PathBuf::from("/data/flatpak/exports/share/icons"),
                PathBuf::from("/var/lib/flatpak/exports/share/icons"),
                PathBuf::from("/home/op/.local/share/flatpak/exports/share/icons"),
            ]
        );
        assert_eq!(roots.unthemed, vec![PathBuf::from("/usr/share/pixmaps")], "the spec's unthemed fallback is searched last and flat");
    }

    #[test]
    fn an_explicit_data_dir_that_repeats_a_flatpak_export_is_not_listed_twice() {
        let env = XdgEnv {
            home: Some("/home/op".to_string()),
            data_dirs: Some("/var/lib/flatpak/exports/share:/usr/share".to_string()),
            ..Default::default()
        };
        let roots = icon_roots(&env);
        let flatpak = PathBuf::from("/var/lib/flatpak/exports/share/icons");
        assert_eq!(roots.themed.iter().filter(|d| **d == flatpak).count(), 1);
    }

    #[test]
    fn no_home_at_all_still_yields_the_system_roots_and_never_a_bare_dot_path() {
        let roots = icon_roots(&XdgEnv::default());
        assert!(roots.themed.contains(&PathBuf::from("/usr/share/icons")));
        assert!(roots.themed.iter().all(|d| !d.starts_with("/.icons") && !d.starts_with("/.local")));
    }

    // ── theme preference ────────────────────────────────────────────────

    #[test]
    fn the_env_override_beats_gtk_which_beats_the_default() {
        let gtk = "[Settings]\ngtk-icon-theme-name=Papirus\n";
        assert_eq!(preferred_theme(Some("Breeze"), Some(gtk)), "Breeze");
        assert_eq!(preferred_theme(None, Some(gtk)), "Papirus");
        assert_eq!(preferred_theme(None, None), DEFAULT_THEME);
        assert_eq!(preferred_theme(Some("  "), Some(gtk)), "Papirus", "a blank override is not an override");
        assert_eq!(preferred_theme(Some("../etc"), None), DEFAULT_THEME, "an override that could escape the roots is refused");
    }

    // ── the chain ───────────────────────────────────────────────────────

    /// A real directory tree, because the chain's whole job is reading
    /// `index.theme` files off disk in precedence order.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("duduclaw-shell-icons-{}-{tag}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).expect("temp dir");
            Self { root }
        }

        fn theme(&self, name: &str, index: &str) {
            let dir = self.root.join("usr/share/icons").join(name);
            std::fs::create_dir_all(&dir).expect("theme dir");
            std::fs::write(dir.join("index.theme"), index).expect("index.theme");
        }

        fn icon(&self, theme: &str, dir: &str, file: &str, bytes: &[u8]) {
            let dir = self.root.join("usr/share/icons").join(theme).join(dir);
            std::fs::create_dir_all(&dir).expect("icon dir");
            std::fs::write(dir.join(file), bytes).expect("icon file");
        }

        fn pixmap(&self, file: &str, bytes: &[u8]) {
            let dir = self.root.join("usr/share/pixmaps");
            std::fs::create_dir_all(&dir).expect("pixmaps");
            std::fs::write(dir.join(file), bytes).expect("pixmap");
        }

        fn roots(&self) -> IconRoots {
            IconRoots {
                themed: vec![self.root.join("usr/share/icons")],
                unthemed: vec![self.root.join("usr/share/pixmaps")],
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"></svg>";

    /// Minimal PNG prefix — header only, which is all `png_info` reads.
    fn png(width: u32, height: u32, color_type: u8) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, color_type, 0, 0, 0]);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"IDAT");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn app_index(dirs: &[(&str, u32)], inherits: Option<&str>) -> String {
        let mut out = String::from("[Icon Theme]\n");
        if let Some(inherits) = inherits {
            out.push_str(&format!("Inherits={inherits}\n"));
        }
        out.push_str(&format!("Directories={}\n\n", dirs.iter().map(|(p, _)| *p).collect::<Vec<_>>().join(",")));
        for (path, size) in dirs {
            let kind = if path.starts_with("scalable") { "Scalable\nMinSize=8\nMaxSize=512" } else { "Fixed" };
            out.push_str(&format!("[{path}]\nSize={size}\nContext=Applications\nType={kind}\n\n"));
        }
        out
    }

    #[test]
    fn the_chain_is_preferred_then_parents_then_hicolor() {
        let fx = Fixture::new("chain");
        fx.theme("Papirus", &app_index(&[("48x48/apps", 48)], Some("Adwaita")));
        fx.theme("Adwaita", &app_index(&[("48x48/apps", 48)], Some("hicolor")));
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        let chain = ThemeChain::load(&fx.roots(), "Papirus");
        assert_eq!(chain.names(), vec!["Papirus", "Adwaita", "hicolor"]);
    }

    #[test]
    fn hicolor_is_always_in_the_chain_even_when_nothing_inherits_it() {
        let fx = Fixture::new("hicolor");
        fx.theme("Lonely", &app_index(&[("48x48/apps", 48)], None));
        let chain = ThemeChain::load(&fx.roots(), "Lonely");
        assert_eq!(chain.names(), vec!["Lonely", "hicolor"], "the spec's universal fallback theme is never optional");
    }

    #[test]
    fn an_inherits_cycle_terminates_instead_of_looping_forever() {
        let fx = Fixture::new("cycle");
        fx.theme("A", &app_index(&[("48x48/apps", 48)], Some("B")));
        fx.theme("B", &app_index(&[("48x48/apps", 48)], Some("A")));
        let chain = ThemeChain::load(&fx.roots(), "A");
        assert_eq!(chain.names(), vec!["A", "B", "hicolor"]);
    }

    #[test]
    fn a_theme_with_no_index_theme_at_all_still_leaves_hicolor_searchable() {
        let fx = Fixture::new("noindex");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        let chain = ThemeChain::load(&fx.roots(), "DoesNotExist");
        assert_eq!(chain.names(), vec!["DoesNotExist", "hicolor"]);
    }

    // ── resolution, end to end on a real tree ───────────────────────────

    #[test]
    fn an_app_with_no_icon_key_is_a_distinct_miss_from_an_unresolvable_one() {
        let fx = Fixture::new("miss");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "Adwaita");
        assert_eq!(resolve(None, &roots, &chain), Err(IconMiss::NoIconKey));
        assert_eq!(resolve(Some("   "), &roots, &chain), Err(IconMiss::NoIconKey));
        assert_eq!(resolve(Some("nothing-answers-to-this"), &roots, &chain), Err(IconMiss::Unresolved));
    }

    #[test]
    fn a_scalable_svg_beats_every_raster_size_at_both_rungs_of_the_ladder() {
        let fx = Fixture::new("svgwins");
        fx.theme("hicolor", &app_index(&[("24x24/apps", 24), ("48x48/apps", 48), ("scalable/apps", 128)], None));
        fx.icon("hicolor", "24x24/apps", "chromium.png", &png(24, 24, 6));
        fx.icon("hicolor", "48x48/apps", "chromium.png", &png(48, 48, 6));
        fx.icon("hicolor", "scalable/apps", "chromium.svg", SVG);
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("chromium"), &roots, &chain).expect("resolves");
        for container in icon_theme::RENDER_CONTAINERS {
            let variant = icon.for_container(container).expect("every rung is resolved");
            assert!(variant.path.ends_with("chromium.svg"), "flatpak conventions: the scalable SVG serves every size");
            assert_eq!(variant.draw_px, icon_theme::content_px(container), "vector art is drawn at the full 80% content box");
            assert!(!variant.full_bleed, "an SVG is never clipped");
        }
    }

    #[test]
    fn each_rung_gets_the_nearest_raster_that_is_not_smaller_than_it() {
        let fx = Fixture::new("ladder");
        fx.theme("hicolor", &app_index(&[("16x16/apps", 16), ("24x24/apps", 24), ("48x48/apps", 48)], None));
        fx.icon("hicolor", "16x16/apps", "gedit.png", &png(16, 16, 6));
        fx.icon("hicolor", "24x24/apps", "gedit.png", &png(24, 24, 6));
        fx.icon("hicolor", "48x48/apps", "gedit.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("gedit"), &roots, &chain).expect("resolves");

        // Launcher row: 30 x 80% = 24 -> the 24px file, exactly.
        let row = icon.for_container(icon_theme::TILE_ROW_PX).expect("row rung");
        assert!(row.path.ends_with("24x24/apps/gedit.png"));
        assert_eq!(row.draw_px, 24.);
        // Dock: 44 x 80% = 35 -> the 48px file scaled DOWN, never the 24.
        let dock = icon.for_container(icon_theme::TILE_DOCK_PX).expect("dock rung");
        assert!(dock.path.ends_with("48x48/apps/gedit.png"));
        assert_eq!(dock.draw_px, 35.);
    }

    #[test]
    fn a_theme_that_only_has_small_rasters_is_drawn_at_its_own_size_never_blown_up() {
        let fx = Fixture::new("small");
        fx.theme("hicolor", &app_index(&[("22x22/apps", 22)], None));
        fx.icon("hicolor", "22x22/apps", "tiny.png", &png(22, 22, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("tiny"), &roots, &chain).expect("resolves");
        let dock = icon.for_container(icon_theme::TILE_DOCK_PX).expect("dock rung");
        assert_eq!(dock.draw_px, 22., "a 22px raster in a 35px slot stays 22px");
    }

    #[test]
    fn the_preferred_theme_wins_and_its_parents_are_only_consulted_when_it_has_nothing() {
        let fx = Fixture::new("precedence");
        fx.theme("Papirus", &app_index(&[("48x48/apps", 48)], Some("hicolor")));
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        fx.icon("Papirus", "48x48/apps", "shared.png", &png(48, 48, 6));
        fx.icon("hicolor", "48x48/apps", "shared.png", &png(48, 48, 6));
        fx.icon("hicolor", "48x48/apps", "only-hicolor.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "Papirus");

        let shared = resolve(Some("shared"), &roots, &chain).expect("resolves");
        assert!(shared.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.to_string_lossy().contains("Papirus"));
        let inherited = resolve(Some("only-hicolor"), &roots, &chain).expect("resolves");
        assert!(inherited.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.to_string_lossy().contains("hicolor"));
    }

    #[test]
    fn the_unthemed_pixmaps_directory_is_the_last_resort() {
        let fx = Fixture::new("pixmaps");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        fx.pixmap("legacy.png", &png(64, 64, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("legacy"), &roots, &chain).expect("resolves");
        assert!(icon.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.ends_with("pixmaps/legacy.png"));
    }

    #[test]
    fn an_absolute_icon_path_is_used_directly_with_no_theme_lookup() {
        let fx = Fixture::new("absolute");
        fx.pixmap("direct.png", &png(64, 64, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let absolute = fx.root.join("usr/share/pixmaps/direct.png");
        let icon = resolve(Some(absolute.to_str().unwrap()), &roots, &chain).expect("resolves");
        assert_eq!(icon.for_container(icon_theme::TILE_DOCK_PX).unwrap().path, absolute);
        // …and one that is not there, or that this shell cannot draw, is an
        // honest miss rather than a tile that paints nothing.
        assert_eq!(resolve(Some("/nope/missing.png"), &roots, &chain), Err(IconMiss::Unresolved));
        fx.pixmap("legacy.xpm", b"/* XPM */");
        let xpm = fx.root.join("usr/share/pixmaps/legacy.xpm");
        assert_eq!(resolve(Some(xpm.to_str().unwrap()), &roots, &chain), Err(IconMiss::Unresolved), "gpui has no XPM decoder");
    }

    #[test]
    fn a_provably_opaque_square_raster_is_the_only_thing_marked_full_bleed() {
        let fx = Fixture::new("fullbleed");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        fx.icon("hicolor", "48x48/apps", "opaque.png", &png(48, 48, 2));
        fx.icon("hicolor", "48x48/apps", "alpha.png", &png(48, 48, 6));
        fx.icon("hicolor", "48x48/apps", "wide.png", &png(48, 24, 2));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let dock = icon_theme::TILE_DOCK_PX;
        assert!(resolve(Some("opaque"), &roots, &chain).unwrap().for_container(dock).unwrap().full_bleed);
        assert!(!resolve(Some("alpha"), &roots, &chain).unwrap().for_container(dock).unwrap().full_bleed);
        assert!(!resolve(Some("wide"), &roots, &chain).unwrap().for_container(dock).unwrap().full_bleed);
    }

    /// The two-phase probe's whole reason for existing: an icon that is NOT
    /// in an application-context directory must still be found. The first
    /// pass looks at the app directories, finds nothing, and the second
    /// sweeps the rest — so `serves_apps` speeds the search up without ever
    /// being able to hide a file.
    #[test]
    fn an_icon_filed_outside_the_application_directories_is_still_found() {
        let fx = Fixture::new("twophase");
        fx.theme(
            "hicolor",
            "[Icon Theme]\nDirectories=48x48/apps,48x48/mimetypes\n\n[48x48/apps]\nSize=48\nContext=Applications\nType=Fixed\n\n[48x48/mimetypes]\nSize=48\nContext=MimeTypes\nType=Fixed\n",
        );
        fx.icon("hicolor", "48x48/mimetypes", "odd-one-out.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("odd-one-out"), &roots, &chain).expect("the second phase sweeps the rest of the theme");
        assert!(icon.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.ends_with("48x48/mimetypes/odd-one-out.png"));
    }

    /// …and when BOTH kinds of directory hold the same name, the
    /// application one wins, because the first phase answers and the search
    /// stops there.
    #[test]
    fn an_application_directory_beats_an_identically_named_icon_elsewhere() {
        let fx = Fixture::new("twophase-precedence");
        fx.theme(
            "hicolor",
            "[Icon Theme]\nDirectories=48x48/apps,48x48/mimetypes\n\n[48x48/apps]\nSize=48\nContext=Applications\nType=Fixed\n\n[48x48/mimetypes]\nSize=48\nContext=MimeTypes\nType=Fixed\n",
        );
        fx.icon("hicolor", "48x48/apps", "shared-name.png", &png(48, 48, 6));
        fx.icon("hicolor", "48x48/mimetypes", "shared-name.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("shared-name"), &roots, &chain).expect("resolves");
        assert!(icon.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.ends_with("48x48/apps/shared-name.png"));
    }

    /// A theme lives in one root; the other six must not be probed for it.
    /// Cheap to assert indirectly: a root that does NOT contain the theme
    /// directory contributes nothing even when it holds a file at the same
    /// relative path.
    #[test]
    fn only_roots_that_actually_contain_the_theme_are_probed() {
        let fx = Fixture::new("rootprune");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        fx.icon("hicolor", "48x48/apps", "here.png", &png(48, 48, 6));
        // A decoy root with the same relative layout but no `hicolor`
        // directory of its own — it is not part of the theme.
        let decoy = fx.root.join("decoy/icons");
        std::fs::create_dir_all(decoy.join("elsewhere/48x48/apps")).expect("decoy");
        std::fs::write(decoy.join("elsewhere/48x48/apps/here.png"), png(64, 64, 6)).unwrap();
        let mut roots = fx.roots();
        roots.themed.push(decoy);
        let chain = ThemeChain::load(&roots, "hicolor");
        let icon = resolve(Some("here"), &roots, &chain).expect("resolves");
        assert!(icon.for_container(icon_theme::TILE_DOCK_PX).unwrap().path.to_string_lossy().contains("usr/share/icons/hicolor"));
    }

    #[test]
    fn a_theme_with_no_context_keys_is_still_searched() {
        // A flat, hand-written theme: no `Context=`, no `apps` leaf. The
        // directory filter must fail OPEN rather than making it invisible.
        let fx = Fixture::new("flat");
        fx.theme("Flat", "[Icon Theme]\nDirectories=48\n\n[48]\nSize=48\nType=Fixed\n");
        fx.icon("Flat", "48", "thing.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "Flat");
        assert!(resolve(Some("thing"), &roots, &chain).is_ok());
    }

    #[test]
    fn an_icon_value_carrying_a_redundant_extension_still_resolves() {
        let fx = Fixture::new("extension");
        fx.theme("hicolor", &app_index(&[("48x48/apps", 48)], None));
        fx.icon("hicolor", "48x48/apps", "thing.png", &png(48, 48, 6));
        let roots = fx.roots();
        let chain = ThemeChain::load(&roots, "hicolor");
        assert!(resolve(Some("thing.png"), &roots, &chain).is_ok(), "`Icon=thing.png` must not be looked up as `thing.png.png`");
    }

    #[test]
    fn png_headers_are_read_off_a_real_file_and_a_missing_one_is_not_a_panic() {
        let fx = Fixture::new("header");
        fx.pixmap("real.png", &png(64, 64, 2));
        let info = read_png_header(&fx.root.join("usr/share/pixmaps/real.png")).expect("a real header");
        assert_eq!((info.width, info.height, info.color_type), (64, 64, 2));
        assert!(read_png_header(&fx.root.join("usr/share/pixmaps/gone.png")).is_none());
        fx.pixmap("actually-an-svg.png", SVG);
        assert!(read_png_header(&fx.root.join("usr/share/pixmaps/actually-an-svg.png")).is_none());
    }

    /// Live-fire, never run by a bare `cargo test` — same `#[ignore]`
    /// contract `apps::installed::live_scan_this_machine` establishes.
    /// Prints what THIS machine's icon themes actually answer:
    ///
    /// ```text
    /// cargo test -- --ignored live_resolve_this_machine --nocapture
    /// ```
    #[test]
    #[ignore]
    fn live_resolve_this_machine() {
        let env = super::super::installed::xdg_env_from_process();
        let roots = icon_roots(&env);
        let chain = theme_chain_for(&env, &roots, std::env::var(THEME_ENV).ok().as_deref());
        eprintln!("[live] icon roots: {:?}", roots.themed);
        eprintln!("[live] theme chain: {:?}", chain.names());
        for app in super::super::installed::scan().apps {
            eprintln!("[live]   {} icon={:?} -> {:?}", app.id, app.icon, app.resolved_icon);
        }
    }
}
