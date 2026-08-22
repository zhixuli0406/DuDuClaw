// Icon Theme Specification — the PURE half. ICON-2 (2026-08-22).
//
// APP-1 parsed and carried every installed app's `.desktop` `Icon=` value
// (`apps/installed.rs`'s `InstalledApp::icon`) without rendering it; the
// icon slot drew the app name's first character instead. That placeholder
// has no precedent on any desktop OS — the 2026-08 research sweep
// (`research/native-os-2026-08/icon-and-cursor-system-2026-08.md` §2.3)
// checked GNOME, KDE, elementary, Windows and Android and found the
// first-letter tile nowhere; it is a CONTACT-avatar convention (Slack,
// Discord, Gmail), not an APP one. All five ship a generic application
// icon instead (GNOME Shell's own `shell-app.c`:
// `g_themed_icon_new ("application-x-executable")`).
//
// This module turns `Icon=` into a real file. It is the spec logic only —
// zero I/O, zero `std::env`, same discipline `apps/desktop_entry.rs` states
// for itself; `apps/icon_resolve.rs` is the half that touches the
// filesystem.
//
// Spec followed: freedesktop.org Icon Theme Specification (`Directories` /
// `Inherits` / `Size` / `Type` / `MinSize` / `MaxSize` / `Threshold` /
// `Scale` / `Context`), plus the Flatpak conventions' "scalable SVG" rule.
//
// ── Two rules this shell adds on top of the spec ────────────────────────
//   1. **Never upscale.** The spec's `FindBestIcon` happily returns a 16px
//      raster for a 48px request; blowing that up to a dock tile looks
//      broken. `pick` asks for the nearest nominal size that is NOT SMALLER
//      than the target, and when nothing that big exists `draw_px` shrinks
//      the DRAWN box to the file's own pixel size rather than stretching
//      it.
//   2. **SVG first.** A `scalable/` SVG is one file that serves every size
//      on the ladder, which is exactly what Flatpak's own conventions say
//      apps should ship.
//
// ── Why the directory hint can never hide an icon ───────────────────────
// Real themes are big: measured on Debian bookworm, hicolor declares 649
// subdirectories and Adwaita 97. Probing two extensions in every one of
// them, for every installed app, on a 60-second refresh, is tens of
// thousands of `stat` calls per scan.
//
// `serves_apps` marks the directories that are LIKELY to hold an
// application icon (`Context=Applications` or `Context=Legacy` — see that
// fn for why Legacy is not a guess — or the freedesktop `apps` leaf
// convention). It is a FIRST PASS, not a filter: `icon_resolve::
// collect_candidates` probes those directories first and, only when they
// answer with nothing, sweeps the theme's remaining directories too. So an
// icon filed somewhere unexpected still resolves; it just costs the slow
// path. That two-phase shape exists because the cheap version of this
// (filter and stop) is exactly the kind of silent, invisible miss this work
// package is about removing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ── The shell's app-tile ladder ─────────────────────────────────────────
// Research §2.4: "Dock 44px（home_dock.rs 現值）／Launcher 結果列 24px",
// with the icon body occupying 80% of its container (Apple HIG's own 10%
// margin / 80% canvas rule). 30 × 0.8 = 24 reproduces the研究's Launcher
// number exactly, so ONE ratio expresses both rungs instead of two
// hand-tuned numbers.
//
// These are CONTAINER sizes (the rounded tile), not icon sizes — the icon
// body is `content_px()` of them. Both values are the ones
// `home/home_dock.rs::dock_app` and `overlay/launcher.rs::app_tile` already
// draw; they live here because this module owns "which sizes do we resolve
// an icon for", and `apps/icon_resolve.rs` resolves one variant per rung so
// that rendering never has to touch the filesystem.

/// The dock's app tile (`home/home_dock.rs::dock_app`).
pub(crate) const TILE_DOCK_PX: f32 = 44.;
/// The Launcher result row's app tile (`overlay/launcher.rs::app_tile`).
pub(crate) const TILE_ROW_PX: f32 = 30.;
/// Apple HIG: "it's best when the image occupies about 80% of the image
/// canvas", leaving a ~10% margin on each side.
pub(crate) const CONTENT_RATIO: f32 = 0.8;
/// Every container size an icon is resolved for, at scan time.
pub(crate) const RENDER_CONTAINERS: [f32; 2] = [TILE_DOCK_PX, TILE_ROW_PX];

/// The icon body's size inside a `container_px` tile.
pub(crate) fn content_px(container_px: f32) -> f32 {
    (container_px * CONTENT_RATIO).round()
}

/// The integer key a container size is stored/looked up under. Rounding
/// once, HERE, is what makes "resolved for 44" and "rendering at 44" the
/// same key without comparing floats anywhere.
pub(crate) fn container_key(container_px: f32) -> u32 {
    container_px.round().max(0.) as u32
}

// ── index.theme ─────────────────────────────────────────────────────────

/// A subdirectory's `Type=`. Defaults to `Threshold` per spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DirKind {
    Fixed,
    Scalable,
    #[default]
    Threshold,
}

/// Everything about a theme subdirectory that decides WHICH SIZE it serves.
/// Split out of `ThemeDir` so a resolved candidate can carry it (it is
/// `Copy`, unlike the directory's path) — `pick` needs the full
/// `Type`/`MinSize`/`MaxSize`/`Threshold` semantics long after the
/// directory listing is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirSizing {
    pub size: u32,
    pub scale: u32,
    pub kind: DirKind,
    pub min_size: u32,
    pub max_size: u32,
    pub threshold: u32,
}

impl DirSizing {
    /// For a file that came from outside any theme — an absolute `Icon=`
    /// path, or `/usr/share/pixmaps`. Size `0` reads as "unknown", which
    /// `pick` treats as smaller than every request, so such a file is only
    /// chosen when it is the only candidate. That is exactly right: the
    /// spec's unthemed fallback is the LAST resort.
    pub const UNKNOWN: Self = Self { size: 0, scale: 1, kind: DirKind::Fixed, min_size: 0, max_size: 0, threshold: 0 };

    /// The largest request this directory's files can serve WITHOUT being
    /// blown up.
    ///
    /// For `Fixed` and `Threshold` this is the nominal `Size`, not the top
    /// of the match window: a `Threshold` directory with `Size=48
    /// Threshold=4` legitimately MATCHES a 50px request per spec, but the
    /// file inside it is still 48 pixels wide, and stretching it is the one
    /// thing this shell's ladder refuses to do. Only `Scalable` really does
    /// serve its whole declared range.
    pub fn max_usable_px(&self) -> u32 {
        match self.kind {
            DirKind::Scalable => self.max_size,
            DirKind::Fixed | DirKind::Threshold => self.size,
        }
    }
}

/// One `[<subdir>]` group of an `index.theme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemeDir {
    /// Path relative to the theme root, e.g. `48x48/apps`.
    pub path: String,
    pub sizing: DirSizing,
    pub context: Option<String>,
}

/// A parsed `index.theme`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ThemeIndex {
    pub inherits: Vec<String>,
    pub dirs: Vec<ThemeDir>,
}

/// How many subdirectories one theme may declare before the rest are
/// ignored — a safety rail against a corrupt or hostile file turning a
/// background refresh into an unbounded walk.
///
/// Measured on Debian bookworm (2026-08-22, in the Linux build container):
/// Adwaita declares **97** directories, hicolor declares **649**. An
/// earlier 512 was therefore NOT the "well above anything real" figure it
/// was written as — it silently truncated hicolor's last 137 entries,
/// among them `scalable/apps`, which is exactly where a flatpak-style
/// scalable app icon lives. Live-fire caught it: `Icon=mini.xterm`
/// resolved to nothing even though `/usr/share/icons/hicolor/scalable/
/// apps/mini.xterm.svg` was sitting right there. 4096 is ~6x the largest
/// real theme measured.
const MAX_DIRS_PER_THEME: usize = 4096;

/// Parses an `index.theme`. Never fails: a file this parser cannot make
/// sense of yields an index with no directories, which resolves no icons —
/// a malformed theme is skipped, never guessed at.
pub(crate) fn parse_index_theme(content: &str) -> ThemeIndex {
    let groups = parse_groups(content);
    let header = groups.get("Icon Theme");
    let inherits = header.and_then(|g| g.get("Inherits")).map(|v| split_commas(v)).unwrap_or_default();
    let mut declared: Vec<String> = Vec::new();
    for key in ["Directories", "ScaledDirectories"] {
        if let Some(value) = header.and_then(|g| g.get(key)) {
            declared.extend(split_commas(value));
        }
    }

    let mut dirs: Vec<ThemeDir> = Vec::new();
    for path in declared {
        if dirs.len() >= MAX_DIRS_PER_THEME {
            break;
        }
        // A declared directory with no group of its own has no `Size`, so
        // there is no honest way to decide whether it serves a 44px
        // request. Skipped rather than defaulted to some invented size.
        let Some(group) = groups.get(path.as_str()) else {
            continue;
        };
        let Some(size) = group.get("Size").and_then(|v| v.trim().parse::<u32>().ok()) else {
            continue;
        };
        if size == 0 {
            continue;
        }
        let scale = group.get("Scale").and_then(|v| v.trim().parse::<u32>().ok()).filter(|s| *s > 0).unwrap_or(1);
        let kind = match group.get("Type").map(|v| v.trim()) {
            Some("Fixed") => DirKind::Fixed,
            // The spec's own key is `Scalable`; `Scaled` appears in the
            // wild (and in the spec's own prose for `DirectorySizeDistance`)
            // so both are accepted rather than silently falling through to
            // the `Threshold` default, which would change the match window.
            Some("Scalable") | Some("Scaled") => DirKind::Scalable,
            _ => DirKind::Threshold,
        };
        let min_size = group.get("MinSize").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(size);
        let max_size = group.get("MaxSize").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(size);
        let threshold = group.get("Threshold").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(2);
        dirs.push(ThemeDir {
            path,
            sizing: DirSizing { size, scale, kind, min_size: min_size.min(max_size), max_size: max_size.max(min_size), threshold },
            context: group.get("Context").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
        });
    }
    ThemeIndex { inherits, dirs }
}

/// `group -> key -> value`, for the INI-shaped subset both `index.theme`
/// and GTK's `settings.ini` use. Deliberately NOT shared with
/// `apps/desktop_entry.rs`'s parser: that one resolves locale-suffixed keys
/// and honours the desktop-entry escape grammar, neither of which applies
/// here, and a shared "flexible" parser would have to do both for everyone.
fn parse_groups(content: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            current = rest.strip_suffix(']').map(|name| name.trim().to_string());
            if let Some(name) = &current {
                out.entry(name.clone()).or_default();
            }
            continue;
        }
        let Some(group) = current.as_deref() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // First writer wins, matching how every INI reader treats a
        // duplicated key in one group.
        out.entry(group.to_string()).or_default().entry(key.trim().to_string()).or_insert_with(|| value.trim().to_string());
    }
    out
}

fn split_commas(value: &str) -> Vec<String> {
    value.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect()
}

/// GTK's `settings.ini` `gtk-icon-theme-name`. Pure; the read lives in
/// `apps/icon_resolve.rs`. This shell has no settings daemon of its own, so
/// the user's GTK preference is the closest thing to "the icon theme this
/// machine is configured for" that can be read without one.
pub(crate) fn gtk_icon_theme_name(settings_ini: &str) -> Option<String> {
    parse_groups(settings_ini)
        .get("Settings")
        .and_then(|g| g.get("gtk-icon-theme-name"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && is_safe_theme_name(v))
}

/// A theme name becomes a PATH COMPONENT, so it must not be able to escape
/// the search roots. Rejects anything with a separator, a `.` component, or
/// a NUL — the same "a config value must never become an unintended path"
/// guard `apps/desktop_entry.rs::exec_to_argv` applies to the launch path.
pub(crate) fn is_safe_theme_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
}

/// Whether a subdirectory is a LIKELY home for an application icon. Purely
/// an ordering/first-pass hint — never a filter that can hide an icon; see
/// this module's header comment and `icon_resolve::collect_candidates`'s
/// two-phase probe.
///
/// `Context=Legacy` counts, and that is not a guess: Debian bookworm's
/// Adwaita files its full-colour, freedesktop-named app icons
/// (`utilities-terminal.png`, …) under `Context=Legacy`, keeping
/// `Context=Applications` for the handful of symbolic ones. Measured in the
/// Linux container on 2026-08-22 — 11 Legacy directories against 7
/// Applications ones.
pub(crate) fn serves_apps(dir: &ThemeDir) -> bool {
    if let Some(context) = &dir.context {
        return context.eq_ignore_ascii_case("Applications") || context.eq_ignore_ascii_case("Legacy");
    }
    // No `Context` key: fall back to the freedesktop layout convention
    // (`<size>/apps/…`). Whole-component equality, never a substring
    // (this crate's coding convention 2) — `apps-extra` is a different
    // directory.
    dir.path.split('/').next_back().is_some_and(|leaf| leaf.eq_ignore_ascii_case("apps"))
}

// ── Spec size matching ──────────────────────────────────────────────────

/// The spec's `DirectoryMatchesSize` — "this directory is FOR icons of this
/// size", which is a wider claim than "it contains a file exactly this big"
/// (see `DirSizing::max_usable_px`).
pub(crate) fn dir_matches_size(sizing: &DirSizing, icon_size: u32, icon_scale: u32) -> bool {
    if sizing.scale != icon_scale {
        return false;
    }
    match sizing.kind {
        DirKind::Fixed => sizing.size == icon_size,
        DirKind::Scalable => sizing.min_size <= icon_size && icon_size <= sizing.max_size,
        DirKind::Threshold => {
            sizing.size.saturating_sub(sizing.threshold) <= icon_size && icon_size <= sizing.size.saturating_add(sizing.threshold)
        }
    }
}

/// The spec's `DirectorySizeDistance`, in scaled pixels.
pub(crate) fn dir_size_distance(sizing: &DirSizing, icon_size: u32, icon_scale: u32) -> u32 {
    let want = icon_size.saturating_mul(icon_scale);
    match sizing.kind {
        DirKind::Fixed => sizing.size.saturating_mul(sizing.scale).abs_diff(want),
        DirKind::Scalable | DirKind::Threshold => {
            let (low, high) = match sizing.kind {
                DirKind::Scalable => (sizing.min_size, sizing.max_size),
                _ => (sizing.size.saturating_sub(sizing.threshold), sizing.size.saturating_add(sizing.threshold)),
            };
            let low = low.saturating_mul(sizing.scale);
            let high = high.saturating_mul(sizing.scale);
            if want < low {
                low - want
            } else {
                want.saturating_sub(high)
            }
        }
    }
}

// ── Candidates ──────────────────────────────────────────────────────────

/// Which of the two RENDERING PATHS a file goes down. This is the
/// distinction the whole module exists around — see
/// `crate::icons::app_icon_element`'s doc comment for why the shell's own
/// monochrome assets and a third party's artwork cannot share one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconKind {
    /// A `scalable/` SVG. One file serves every rung of the ladder.
    Scalable,
    /// A raster (PNG). Has a real pixel size, and must never be blown up
    /// past it.
    Raster,
}

/// The file extensions this shell will actually select.
///
/// Deliberately NOT the icon-theme spec's full list: that list includes
/// `.xpm`, and `gpui::img()`'s decoder set (`image::ImageFormat` + SVG,
/// `gpui/src/elements/img.rs::Img::extensions`) has no XPM decoder at all.
/// Selecting one would resolve happily and then paint NOTHING — the silent
/// blank hole this work package exists to remove. Ordered: SVG first, so
/// the collection order already expresses the "scalable wins" preference.
pub(crate) const ICON_EXTENSIONS: [&str; 2] = ["svg", "png"];

pub(crate) fn kind_for_extension(extension: &str) -> Option<IconKind> {
    match extension.to_ascii_lowercase().as_str() {
        // `.svgz` (gzipped SVG) is deliberately absent: `gpui::img()`
        // dispatches to the SVG renderer only after `image::guess_format`
        // fails, and nothing on that path un-gzips first — so it would
        // resolve and then paint nothing.
        "svg" => Some(IconKind::Scalable),
        "png" => Some(IconKind::Raster),
        _ => None,
    }
}

/// One file that could serve as an app's icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IconCandidate {
    pub path: PathBuf,
    pub kind: IconKind,
    /// The size semantics of the theme directory it came from, or
    /// `DirSizing::UNKNOWN` for a file that came from outside any theme.
    pub sizing: DirSizing,
}

impl IconCandidate {
    /// The file's declared pixel size — the theme directory's `Size=`.
    pub fn nominal(&self) -> u32 {
        self.sizing.size
    }
}

/// The scale factor every lookup asks for. This shell resolves icons once,
/// at scan time, with no window attached and therefore no way to know which
/// display a tile will end up on; `1` is the size the spec's own `Size=`
/// values are expressed in, and gpui does its own HiDPI scaling on top of
/// whatever bitmap it is handed. Asking for scale 2 would silently skip
/// every ordinary `48x48/apps` directory (they declare `Scale=1`), which is
/// most of what exists.
const LOOKUP_SCALE: u32 = 1;

/// Which candidate to draw at `target_px`.
///
/// Three stages, in the spec's own shape plus this module's never-upscale
/// rule layered on top:
///
///   0. A scalable SVG wins outright — one file serves every rung of the
///      ladder, which is what Flatpak's conventions tell apps to ship.
///   1. **The spec's own answer, filtered.** Directories that
///      `dir_matches_size` says are FOR this size, and whose files are big
///      enough to fill it (`DirSizing::max_usable_px`). Ordered by the
///      spec's `dir_size_distance`, ties to the smaller file.
///   2. **Never upscale.** Any directory whose files are big enough, again
///      ordered by `dir_size_distance`. This is the stage that answers a
///      35px request out of a `48x48` directory, which stage 1 rejects
///      (a `Fixed` 48 directory does not "match" 35 in the spec's sense).
///   3. **Nothing is big enough.** The spec's minimal `dir_size_distance`,
///      ties to the LARGER file. Whatever comes back is smaller than the
///      request, so `draw_px` shrinks the drawn box to the file rather than
///      stretching the file to the box.
///
/// Every stage's tie-break ends at the first candidate in list order, which
/// the resolver produces deterministically (theme, then directory, then
/// extension) — so the same machine always picks the same file.
pub(crate) fn pick(candidates: &[IconCandidate], target_px: u32) -> Option<&IconCandidate> {
    if let Some(svg) = candidates.iter().find(|c| c.kind == IconKind::Scalable) {
        return Some(svg);
    }
    let big_enough = |c: &&IconCandidate| c.sizing.max_usable_px() >= target_px;
    let closest_then_smallest = |c: &&IconCandidate| (dir_size_distance(&c.sizing, target_px, LOOKUP_SCALE), c.nominal());

    candidates
        .iter()
        .filter(|c| big_enough(c) && dir_matches_size(&c.sizing, target_px, LOOKUP_SCALE))
        .min_by_key(closest_then_smallest)
        .or_else(|| candidates.iter().filter(big_enough).min_by_key(closest_then_smallest))
        .or_else(|| {
            candidates
                .iter()
                .min_by_key(|c| (dir_size_distance(&c.sizing, target_px, LOOKUP_SCALE), u32::MAX - c.nominal()))
        })
}

/// How big to DRAW the chosen file inside a `content_px` box. Never larger
/// than the file's own pixels: a 22px raster in a 35px slot is drawn at
/// 22px and centred, not stretched to 35 and blurred.
///
/// `intrinsic_px` is the raster's real width from its own header
/// (`png_info`), which is more trustworthy than the theme directory's
/// nominal `Size=` — a `48x48/apps` directory containing a 32px file is a
/// packaging mistake that happens. `None` (header unreadable, or a
/// scalable SVG) falls back to the nominal size, and a scalable icon is
/// simply drawn at the target.
pub(crate) fn draw_px(target_px: f32, candidate: &IconCandidate, intrinsic_px: Option<u32>) -> f32 {
    if candidate.kind == IconKind::Scalable {
        return target_px;
    }
    let native = intrinsic_px.filter(|p| *p > 0).or_else(|| Some(candidate.nominal())).filter(|p| *p > 0);
    match native {
        Some(px) => target_px.min(px as f32),
        // Neither a readable header nor a nominal size: the honest answer
        // is "draw it at the requested size" — `ObjectFit::Contain` still
        // keeps its aspect ratio, and refusing to draw a file that exists
        // would be worse than drawing it slightly too large.
        None => target_px,
    }
}

// ── PNG header / the "full-bleed square" judgement ──────────────────────

/// The subset of a PNG's `IHDR` (plus "does a `tRNS` chunk exist") that the
/// full-bleed judgement needs. Parsed from the file's first few hundred
/// bytes — never a full decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PngInfo {
    pub width: u32,
    pub height: u32,
    /// PNG `IHDR` colour type: 0 grey, 2 truecolour, 3 palette, 4 grey+α,
    /// 6 truecolour+α.
    pub color_type: u8,
    /// A `tRNS` chunk was found before the first `IDAT`. For colour types
    /// 0/2/3 this is the ONLY way transparency can be present.
    pub has_trns: bool,
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// How many chunk headers to walk looking for `tRNS` before giving up.
/// Ancillary chunks precede `IDAT`; a file that buries one past this many
/// is answered with `has_trns: false` — which is why `is_full_bleed_square`
/// treats an INCOMPLETE walk as "cannot prove it" (see there).
const MAX_PNG_CHUNKS: usize = 32;

/// Parses just enough of a PNG header to judge it. `None` for anything that
/// is not a PNG (an SVG, a truncated read, a JPEG someone named `.png`).
pub(crate) fn png_info(bytes: &[u8]) -> Option<PngInfo> {
    if bytes.len() < 8 + 8 + 13 || bytes[..8] != PNG_SIGNATURE {
        return None;
    }
    // First chunk must be IHDR: length(4) type(4) data(13) crc(4).
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let color_type = bytes[25];

    // Walk the chunk chain looking for `tRNS`, stopping at the first
    // `IDAT` (no ancillary chunk that affects transparency may follow it).
    let mut offset = 8usize;
    let mut has_trns = false;
    let mut walked = 0usize;
    while walked < MAX_PNG_CHUNKS {
        walked += 1;
        let Some(header) = bytes.get(offset..offset + 8) else {
            break;
        };
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = &header[4..8];
        if kind == b"tRNS" {
            has_trns = true;
            break;
        }
        if kind == b"IDAT" || kind == b"IEND" {
            break;
        }
        let Some(next) = offset.checked_add(12).and_then(|o| o.checked_add(length)) else {
            break;
        };
        offset = next;
    }

    Some(PngInfo { width, height, color_type, has_trns })
}

/// Whether this file may be clipped to the tile's own corner radius.
///
/// Research §2.4 says the container is a SOFT mask: it never crops the
/// artwork, "只有當圖示本身是滿版方形點陣圖時才裁圓角". That rule needs a
/// judgement, and the judgement has to be conservative in a specific
/// direction: not clipping a full-bleed icon leaves it looking slightly
/// boxy, while clipping a free-form one cuts a corner off somebody else's
/// logo. So this returns `true` only on POSITIVE PROOF, from the file's own
/// header, that the image is both square and opaque edge to edge:
///
///   * it is a raster (an SVG is free-form vector art by definition — the
///     Inkscape triangle and the GIMP fox the research names are exactly
///     this case), and
///   * `width == height`, and
///   * its colour type carries NO alpha channel (0 grey / 2 truecolour /
///     3 palette — types 4 and 6 always can be transparent), and
///   * no `tRNS` chunk was found, which is the only remaining way types
///     0/2/3 can be transparent.
///
/// Anything else — an RGBA icon (the overwhelming majority), a header that
/// could not be read, a non-square raster — is "cannot prove it", and
/// cannot-prove-it means DO NOT CLIP.
pub(crate) fn is_full_bleed_square(kind: IconKind, png: Option<&PngInfo>) -> bool {
    if kind != IconKind::Raster {
        return false;
    }
    let Some(info) = png else {
        return false;
    };
    info.width > 0 && info.width == info.height && !info.has_trns && matches!(info.color_type, 0 | 2 | 3)
}

// ── `Icon=` value shapes ────────────────────────────────────────────────

/// Strips a redundant extension from a non-absolute `Icon=` value. The spec
/// says the value "should be an icon name, not an icon path" with no
/// extension, but `Icon=foo.png` is common enough in the wild that GTK
/// tolerates it; a lookup for the literal name `foo.png` would find
/// `foo.png.png`.
pub(crate) fn theme_icon_name(icon_value: &str) -> Option<String> {
    let value = icon_value.trim();
    if value.is_empty() || value.contains('/') || value.contains('\0') {
        return None;
    }
    let stem = Path::new(value)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| kind_for_extension(e).is_some() || e.eq_ignore_ascii_case("xpm"))
        .and_then(|_| Path::new(value).file_stem().and_then(|s| s.to_str()))
        .unwrap_or(value);
    let stem = stem.trim();
    if stem.is_empty() || stem == "." || stem == ".." {
        return None;
    }
    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizing(size: u32, kind: DirKind) -> DirSizing {
        DirSizing { size, scale: 1, kind, min_size: size, max_size: size, threshold: 2 }
    }

    fn dir(path: &str, size: u32, kind: DirKind) -> ThemeDir {
        ThemeDir { path: path.to_string(), sizing: sizing(size, kind), context: None }
    }

    /// A raster candidate out of a `Fixed` directory of `nominal` px — the
    /// ordinary shape of a themed PNG.
    fn candidate(name: &str, kind: IconKind, nominal: u32) -> IconCandidate {
        IconCandidate { path: PathBuf::from(name), kind, sizing: sizing(nominal, DirKind::Fixed) }
    }

    // ── the ladder ──────────────────────────────────────────────────────

    #[test]
    fn the_content_ratio_reproduces_the_researchs_own_numbers() {
        // Research §2.4's ladder: dock 44 container, Launcher row 24 icon.
        assert_eq!(content_px(TILE_ROW_PX), 24., "30px tile x 80% is the research's Launcher figure");
        assert_eq!(content_px(TILE_DOCK_PX), 35., "44px tile x 80%, rounded");
        assert_eq!(container_key(TILE_DOCK_PX), 44);
        assert_eq!(container_key(TILE_ROW_PX), 30);
        assert_eq!(RENDER_CONTAINERS.len(), 2);
    }

    // ── index.theme ─────────────────────────────────────────────────────

    const ADWAITA_ISH: &str = "\
# a comment
[Icon Theme]
Name=Adwaita
Comment=The Only One
Inherits=hicolor, gnome
Directories=16x16/apps,48x48/apps,scalable/apps,symbolic/apps
ScaledDirectories=48x48@2/apps

[16x16/apps]
Size=16
Context=Applications
Type=Fixed

[48x48/apps]
Size=48
Context=Applications
Type=Threshold
Threshold=4

[48x48@2/apps]
Size=48
Scale=2
Context=Applications
Type=Fixed

[scalable/apps]
Size=128
MinSize=8
MaxSize=512
Context=Applications
Type=Scalable

[symbolic/apps]
Size=16
Context=Applications
Type=Scalable
MinSize=8
MaxSize=512

[not/declared]
Size=99
";

    #[test]
    fn parses_the_header_and_every_declared_directory() {
        let index = parse_index_theme(ADWAITA_ISH);
        assert_eq!(index.inherits, vec!["hicolor", "gnome"], "Inherits is COMMA separated and trimmed");
        let paths: Vec<&str> = index.dirs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["16x16/apps", "48x48/apps", "scalable/apps", "symbolic/apps", "48x48@2/apps"]);
        assert!(!paths.contains(&"not/declared"), "a group that Directories= never names is not a theme directory");

        let scalable = index.dirs.iter().find(|d| d.path == "scalable/apps").expect("declared");
        assert_eq!(scalable.sizing.kind, DirKind::Scalable);
        assert_eq!((scalable.sizing.min_size, scalable.sizing.max_size, scalable.sizing.size), (8, 512, 128));
        assert_eq!(scalable.sizing.max_usable_px(), 512, "only a Scalable directory really serves its whole range");
        let fixed = index.dirs.iter().find(|d| d.path == "16x16/apps").expect("declared");
        assert_eq!(fixed.sizing.kind, DirKind::Fixed);
        assert_eq!(fixed.sizing.threshold, 2, "the spec's default Threshold");
        let scaled = index.dirs.iter().find(|d| d.path == "48x48@2/apps").expect("ScaledDirectories are directories too");
        assert_eq!(scaled.sizing.scale, 2);
    }

    #[test]
    fn a_declared_directory_with_no_group_or_no_size_is_skipped_not_invented() {
        let index = parse_index_theme("[Icon Theme]\nDirectories=a,b,c\n\n[a]\nSize=16\n\n[b]\nContext=Applications\n");
        assert_eq!(index.dirs.len(), 1, "`b` has no Size and `c` has no group at all");
        assert_eq!(index.dirs[0].path, "a");
    }

    /// Real hicolor on Debian bookworm declares 649 directories, with
    /// `scalable/apps` — the one a flatpak-style scalable icon lives in —
    /// dead last. A cap below that silently truncates it, which is how the
    /// live-fire `Icon=mini.xterm` miss happened. This pins the size of a
    /// real theme against the cap.
    #[test]
    fn a_theme_the_size_of_real_hicolor_is_parsed_whole() {
        const REAL_HICOLOR_DIRS: usize = 649;
        let names: Vec<String> = (0..REAL_HICOLOR_DIRS).map(|i| format!("d{i}/apps")).collect();
        let mut index = format!("[Icon Theme]\nDirectories={}\n", names.join(","));
        for (i, name) in names.iter().enumerate() {
            index.push_str(&format!("\n[{name}]\nSize={}\nContext=Applications\nType=Fixed\n", 16 + i % 64));
        }
        let parsed = parse_index_theme(&index);
        assert_eq!(parsed.dirs.len(), REAL_HICOLOR_DIRS, "a real theme must not be truncated");
        assert_eq!(parsed.dirs.last().map(|d| d.path.as_str()), Some(names[REAL_HICOLOR_DIRS - 1].as_str()), "including its LAST directory");
        // Compile-time on purpose (also what clippy::assertions_on_constants
        // demands): both sides are constants, so "the safety rail sits above
        // the largest theme actually measured" is checkable before any test
        // runs — a lowered MAX_DIRS_PER_THEME fails the build, not a test run.
        const _: () = assert!(MAX_DIRS_PER_THEME > REAL_HICOLOR_DIRS, "the safety rail must sit well above the largest theme actually measured");
    }

    #[test]
    fn a_malformed_or_empty_index_yields_no_directories_rather_than_a_guess() {
        assert_eq!(parse_index_theme(""), ThemeIndex::default());
        assert_eq!(parse_index_theme("not an ini at all\n"), ThemeIndex::default());
        assert_eq!(parse_index_theme("[Icon Theme]\nDirectories=\n"), ThemeIndex::default());
    }

    #[test]
    fn both_spellings_of_the_scalable_type_are_accepted() {
        let index = parse_index_theme("[Icon Theme]\nDirectories=a,b\n\n[a]\nSize=8\nType=Scalable\nMaxSize=64\n\n[b]\nSize=8\nType=Scaled\nMaxSize=64\n");
        assert_eq!(index.dirs[0].sizing.kind, DirKind::Scalable);
        assert_eq!(index.dirs[1].sizing.kind, DirKind::Scalable, "`Scaled` appears in the wild and must not silently become Threshold");
    }

    #[test]
    fn gtk_settings_ini_yields_the_configured_theme_name() {
        assert_eq!(gtk_icon_theme_name("[Settings]\ngtk-theme-name=Adw\ngtk-icon-theme-name=Papirus\n").as_deref(), Some("Papirus"));
        assert_eq!(gtk_icon_theme_name("[Settings]\n").as_deref(), None);
        assert_eq!(gtk_icon_theme_name("").as_deref(), None);
        assert_eq!(gtk_icon_theme_name("[Other]\ngtk-icon-theme-name=Nope\n").as_deref(), None, "the key only counts inside [Settings]");
        assert_eq!(gtk_icon_theme_name("[Settings]\ngtk-icon-theme-name=../../etc\n").as_deref(), None, "a theme name becomes a path component");
    }

    #[test]
    fn theme_names_that_could_escape_the_search_roots_are_refused() {
        assert!(is_safe_theme_name("Adwaita"));
        assert!(is_safe_theme_name("Papirus-Dark"));
        assert!(!is_safe_theme_name(""));
        assert!(!is_safe_theme_name("  "));
        assert!(!is_safe_theme_name(".."));
        assert!(!is_safe_theme_name("a/b"));
        assert!(!is_safe_theme_name("a\\b"));
        assert!(!is_safe_theme_name(&"x".repeat(200)));
    }

    // ── directory filtering ─────────────────────────────────────────────

    #[test]
    fn serves_apps_prefers_the_context_key_and_falls_back_to_the_layout() {
        let mut d = dir("48x48/apps", 48, DirKind::Fixed);
        d.context = Some("Applications".to_string());
        assert!(serves_apps(&d));
        d.context = Some("Legacy".to_string());
        assert!(serves_apps(&d), "Debian's Adwaita files its full-colour app icons under Context=Legacy");
        d.context = Some("MimeTypes".to_string());
        assert!(!serves_apps(&d), "an explicit non-app Context is not a first-pass directory");
        d.context = None;
        assert!(serves_apps(&d), "no Context: the `apps` leaf is the freedesktop convention");
        assert!(!serves_apps(&dir("48x48/mimetypes", 48, DirKind::Fixed)));
        assert!(!serves_apps(&dir("48x48/apps-extra", 48, DirKind::Fixed)), "whole-component match, never a substring");
    }

    // ── spec size matching ──────────────────────────────────────────────

    #[test]
    fn fixed_directories_match_only_their_exact_size() {
        let d = sizing(48, DirKind::Fixed);
        assert!(dir_matches_size(&d, 48, 1));
        assert!(!dir_matches_size(&d, 47, 1));
        assert!(!dir_matches_size(&d, 48, 2), "a scale mismatch never matches");
        assert_eq!(dir_size_distance(&d, 48, 1), 0);
        assert_eq!(dir_size_distance(&d, 35, 1), 13);
    }

    #[test]
    fn threshold_directories_match_within_their_window() {
        let mut d = sizing(48, DirKind::Threshold);
        d.threshold = 4;
        assert!(dir_matches_size(&d, 44, 1));
        assert!(dir_matches_size(&d, 52, 1));
        assert!(!dir_matches_size(&d, 43, 1));
        assert_eq!(dir_size_distance(&d, 48, 1), 0);
        assert_eq!(dir_size_distance(&d, 60, 1), 8, "distance is measured from the window edge, not the nominal size");
        assert_eq!(dir_size_distance(&d, 30, 1), 14);
        assert_eq!(d.max_usable_px(), 48, "the window widens what MATCHES, never what the file actually contains");
    }

    #[test]
    fn scalable_directories_match_their_whole_range() {
        let mut d = sizing(128, DirKind::Scalable);
        d.min_size = 8;
        d.max_size = 512;
        assert!(dir_matches_size(&d, 35, 1));
        assert!(dir_matches_size(&d, 512, 1));
        assert!(!dir_matches_size(&d, 513, 1));
        assert_eq!(dir_size_distance(&d, 35, 1), 0);
        assert_eq!(dir_size_distance(&d, 4, 1), 4);
    }

    #[test]
    fn a_min_max_pair_written_backwards_is_normalised_rather_than_panicking() {
        let index = parse_index_theme("[Icon Theme]\nDirectories=a\n\n[a]\nSize=48\nType=Scalable\nMinSize=512\nMaxSize=8\n");
        let d = &index.dirs[0].sizing;
        assert!(d.min_size <= d.max_size);
        assert!(dir_matches_size(d, 48, 1));
    }

    // ── candidate selection ─────────────────────────────────────────────

    #[test]
    fn a_scalable_svg_always_wins() {
        let candidates =
            vec![candidate("a.png", IconKind::Raster, 48), candidate("b.svg", IconKind::Scalable, 128), candidate("c.png", IconKind::Raster, 512)];
        assert_eq!(pick(&candidates, 35).map(|c| c.path.as_path()), Some(Path::new("b.svg")));
        assert_eq!(pick(&candidates, 24).map(|c| c.path.as_path()), Some(Path::new("b.svg")));
    }

    #[test]
    fn the_nearest_size_that_is_not_smaller_is_chosen() {
        let candidates = vec![
            candidate("16.png", IconKind::Raster, 16),
            candidate("24.png", IconKind::Raster, 24),
            candidate("48.png", IconKind::Raster, 48),
            candidate("256.png", IconKind::Raster, 256),
        ];
        assert_eq!(pick(&candidates, 24).map(|c| c.nominal()), Some(24), "an exact match is 'not smaller'");
        assert_eq!(pick(&candidates, 35).map(|c| c.nominal()), Some(48), "35 must not be served by the 24px file");
        assert_eq!(pick(&candidates, 49).map(|c| c.nominal()), Some(256));
    }

    #[test]
    fn when_everything_is_smaller_the_largest_is_chosen_and_never_blown_up() {
        let candidates = vec![candidate("16.png", IconKind::Raster, 16), candidate("22.png", IconKind::Raster, 22)];
        let chosen = pick(&candidates, 35).expect("something is better than nothing");
        assert_eq!(chosen.nominal(), 22, "the closest of the too-small files, not the smallest");
        assert_eq!(draw_px(35., chosen, Some(22)), 22., "a 22px raster is drawn at 22px in a 35px slot, never stretched");
    }

    #[test]
    fn draw_px_trusts_the_real_pixel_size_over_the_directorys_nominal_one() {
        let mispackaged = candidate("48/x.png", IconKind::Raster, 48);
        assert_eq!(draw_px(35., &mispackaged, Some(32)), 32., "a 32px file in a 48x48 directory is still only 32px");
        assert_eq!(draw_px(35., &mispackaged, None), 35., "no header: fall back to the nominal size, which covers the target");
        assert_eq!(draw_px(35., &candidate("x.svg", IconKind::Scalable, 128), None), 35., "vector art has no upscaling to avoid");
        assert_eq!(draw_px(35., &candidate("x.png", IconKind::Raster, 0), None), 35., "neither header nor nominal: draw it rather than refuse");
    }

    #[test]
    fn an_empty_candidate_list_picks_nothing() {
        assert!(pick(&[], 35).is_none());
    }

    // ── PNG header ──────────────────────────────────────────────────────

    /// Builds a minimal PNG prefix: signature + IHDR, optionally a `tRNS`
    /// chunk, then an `IDAT` terminator. Only the header bytes matter —
    /// `png_info` never decodes pixels.
    fn png_bytes(width: u32, height: u32, color_type: u8, trns: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, color_type, 0, 0, 0]);
        out.extend_from_slice(&[0, 0, 0, 0]); // CRC (never checked)
        if trns {
            out.extend_from_slice(&2u32.to_be_bytes());
            out.extend_from_slice(b"tRNS");
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"IDAT");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    #[test]
    fn png_headers_are_read_without_decoding_the_image() {
        let info = png_info(&png_bytes(48, 48, 6, false)).expect("a real PNG header");
        assert_eq!((info.width, info.height, info.color_type, info.has_trns), (48, 48, 6, false));
        let paletted = png_info(&png_bytes(64, 32, 3, true)).expect("a real PNG header");
        assert_eq!((paletted.width, paletted.height, paletted.color_type, paletted.has_trns), (64, 32, 3, true));
    }

    #[test]
    fn anything_that_is_not_a_png_yields_no_header() {
        assert!(png_info(b"").is_none());
        assert!(png_info(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").is_none());
        assert!(png_info(&png_bytes(48, 48, 6, false)[..20]).is_none(), "a truncated read is not a header");
        let mut wrong_first_chunk = png_bytes(48, 48, 6, false);
        wrong_first_chunk[12..16].copy_from_slice(b"pHYs");
        assert!(png_info(&wrong_first_chunk).is_none(), "IHDR must be the first chunk");
    }

    // ── the full-bleed judgement ────────────────────────────────────────

    #[test]
    fn only_a_provably_opaque_square_raster_may_be_clipped() {
        for color_type in [0u8, 2, 3] {
            let info = png_info(&png_bytes(48, 48, color_type, false)).unwrap();
            assert!(is_full_bleed_square(IconKind::Raster, Some(&info)), "colour type {color_type} carries no alpha channel — provably opaque");
        }
    }

    #[test]
    fn everything_we_cannot_prove_is_left_uncropped() {
        // The overwhelming majority: RGBA.
        let rgba = png_info(&png_bytes(48, 48, 6, false)).unwrap();
        assert!(!is_full_bleed_square(IconKind::Raster, Some(&rgba)), "an alpha channel could be transparent anywhere, including the corners");
        // Grey + alpha.
        let grey_alpha = png_info(&png_bytes(48, 48, 4, false)).unwrap();
        assert!(!is_full_bleed_square(IconKind::Raster, Some(&grey_alpha)));
        // Opaque colour type, but a colour-key tRNS chunk.
        let keyed = png_info(&png_bytes(48, 48, 2, true)).unwrap();
        assert!(!is_full_bleed_square(IconKind::Raster, Some(&keyed)));
        // Not square.
        let wide = png_info(&png_bytes(64, 48, 2, false)).unwrap();
        assert!(!is_full_bleed_square(IconKind::Raster, Some(&wide)));
        // No header at all.
        assert!(!is_full_bleed_square(IconKind::Raster, None));
        // Vector art is never clipped, whatever its header says.
        let square = png_info(&png_bytes(48, 48, 2, false)).unwrap();
        assert!(!is_full_bleed_square(IconKind::Scalable, Some(&square)), "an SVG is free-form artwork — clipping it cuts someone's logo");
    }

    // ── Icon= value shapes ──────────────────────────────────────────────

    #[test]
    fn a_redundant_extension_is_stripped_from_a_theme_icon_name() {
        assert_eq!(theme_icon_name("chromium").as_deref(), Some("chromium"));
        assert_eq!(theme_icon_name("chromium.png").as_deref(), Some("chromium"));
        assert_eq!(theme_icon_name("chromium.svg").as_deref(), Some("chromium"));
        assert_eq!(theme_icon_name("chromium.xpm").as_deref(), Some("chromium"), "an xpm-named value still names the same icon");
        assert_eq!(theme_icon_name("org.chromium.Chromium").as_deref(), Some("org.chromium.Chromium"), "a reverse-DNS id is NOT an extension");
    }

    #[test]
    fn a_path_shaped_or_empty_icon_value_is_not_a_theme_name() {
        assert_eq!(theme_icon_name("/usr/share/pixmaps/x.png"), None, "an absolute path never goes through theme lookup");
        assert_eq!(theme_icon_name("../evil"), None);
        assert_eq!(theme_icon_name(""), None);
        assert_eq!(theme_icon_name("   "), None);
        assert_eq!(theme_icon_name(".."), None);
    }

    /// The load-bearing one for the third-party rendering path: a format
    /// this module is willing to SELECT but `gpui::img()` cannot DECODE
    /// resolves happily and then paints nothing — a silent blank hole, the
    /// exact failure mode this work package exists to remove. Checked
    /// against gpui's own published list rather than a copy of it.
    #[test]
    fn the_extension_list_never_offers_a_format_gpui_cannot_draw() {
        let decodable = gpui::Img::extensions();
        for extension in ICON_EXTENSIONS {
            assert!(kind_for_extension(extension).is_some());
            assert!(decodable.iter().any(|e| e.eq_ignore_ascii_case(extension)), "gpui's img() cannot decode .{extension}");
        }
        assert!(kind_for_extension("xpm").is_none(), "gpui has no XPM decoder — selecting one would paint nothing");
        assert!(!decodable.iter().any(|e| e.eq_ignore_ascii_case("xpm")), "if gpui ever gains an XPM decoder, revisit ICON_EXTENSIONS");
        assert!(kind_for_extension("PNG").is_some(), "extensions are matched case-insensitively");
    }
}
