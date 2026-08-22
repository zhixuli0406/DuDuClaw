//! CUR-1: loading standard XCursor theme artwork for the human pointer.
//!
//! smithay 0.7.0 has **no** cursor-theme loader of its own (`grep -rn
//! xcursor` over the published crate returns nothing); anvil, its reference
//! compositor, carries its own `xcursor`-crate-based one. This module is that
//! piece for `duduclaw-comp`, kept deliberately small:
//!
//! 1. Ask `xcursor::CursorTheme` for the file behind an icon name
//!    (`/usr/share/icons/<theme>/cursors/<name>`, following the theme's
//!    `Inherits` chain — the crate does that walk for us).
//! 2. Parse it (`xcursor::parser::parse_xcursor`), pick the image whose
//!    nominal size is closest to the requested one.
//! 3. Wrap its pixels in a `MemoryRenderBuffer` and remember the hotspot.
//!
//! Everything is cached per icon name for the process lifetime: a cursor
//! theme does not change under a running compositor, and re-reading + re-
//! parsing a file on every pointer motion would be absurd.
//!
//! # Honest limitations (see also BUILD.md's CUR-1 section)
//!
//! * **Animated cursors show their first frame only.** XCursor files store
//!   every frame of e.g. `wait` at the same nominal size; this picks the
//!   first and ignores `delay`. A spinning "busy" cursor therefore renders as
//!   a static one. Real animation needs a per-frame timer feeding
//!   `queue_redraw`, which is a scheduling change, not a loading one.
//! * **No scalable (SVG) cursors.** `xcursor` 0.3 can locate
//!   `cursors_scalable/` entries but explicitly leaves rendering them to the
//!   caller; that would mean pulling in an SVG rasteriser. Raster themes are
//!   what ships on the appliance image.
//! * **No fractional/HiDPI scaling.** The whole compositor renders at scale
//!   1.0 today (`render_output(…, 1.0, …)` in both backends), so the cursor
//!   agrees with everything else on screen. `XCURSOR_SIZE` still lets an
//!   operator pick a bigger cursor.

use std::collections::HashMap;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::CursorIcon,
    utils::{Logical, Point, Transform},
};

use super::{
    fallback,
    source::{self, CursorSource},
};

/// One ready-to-draw cursor image.
///
/// `MemoryRenderBuffer` is an `Arc`-backed handle (smithay derives `Clone`
/// on it), so cloning one out of the cache is cheap AND keeps the same
/// texture id — the renderer's per-buffer texture cache stays warm across
/// frames instead of re-uploading the image every redraw.
#[derive(Debug, Clone)]
pub struct LoadedCursor {
    pub buffer: MemoryRenderBuffer,
    /// Where inside the image the pointer's actual position sits, in logical
    /// pixels from the image's top-left.
    pub hotspot: Point<i32, Logical>,
}

/// Loads and caches cursor images for one theme.
pub struct CursorThemeStore {
    /// What is actually in effect, after the brand→system fail-safe.
    source: CursorSource,
    /// CUR-2: what was ASKED for. Differs from `source` exactly when the
    /// brand theme was requested and is not installed. Both are reported to
    /// `shell_control` callers: a settings UI must keep showing the user's
    /// own choice (`requested`) while being able to say what is really being
    /// drawn (`source`).
    requested: CursorSource,
    theme_name: String,
    size: u32,
    /// CUR-2: the env-derived inputs to `resolve_theme_name`, kept so a live
    /// source switch can re-resolve without re-reading the environment. It
    /// must not re-read: `std::env::var` in a running compositor would pick
    /// up nothing new (a process's own environment does not change) while
    /// making the function untestable, and re-reading would silently move
    /// the "explicit override wins" rule from startup to switch time.
    explicit_theme: Option<String>,
    xcursor_theme: Option<String>,
    theme: xcursor::CursorTheme,
    /// Keyed by [`CursorIcon::name`] (a `&'static str`), so no allocation and
    /// no `Hash` requirement on `CursorIcon`. `None` means "we already looked
    /// and this theme has nothing for that icon" — a negative cache, so a
    /// missing icon costs one filesystem walk, not one per frame.
    cache: HashMap<&'static str, Option<LoadedCursor>>,
    /// Built on first use, then reused. `None` until then.
    fallback: Option<LoadedCursor>,
    /// Whether the "no usable theme" warning has already been emitted, so a
    /// degraded run logs its reason once instead of once per pointer motion.
    degraded_logged: bool,
}

impl CursorThemeStore {
    /// Reads the environment, loads the theme, and reports what it got.
    ///
    /// Never fails: a machine with no cursor theme at all still gets a
    /// working (fallback) pointer. It does log — loudly enough that a
    /// degraded appliance is diagnosable from `journalctl` alone, which is
    /// requirement 3 of the CUR-1 brief ("降級要記一行 log 說明為什麼").
    pub fn from_env() -> Self {
        // CUR-2: the source is no longer read from the env var alone — a
        // stored user preference sits between the env var and the default.
        // See `cursor/persist.rs` for the whole priority order and why the
        // env var still wins.
        let env_raw = std::env::var(source::CURSOR_SOURCE_ENV).ok();
        let (requested, origin) =
            source::resolve_startup_source(env_raw.as_deref(), super::persist::load());
        let explicit = std::env::var(source::CURSOR_THEME_ENV).ok();
        let xcursor_theme = std::env::var("XCURSOR_THEME").ok();
        let size = source::resolve_size(std::env::var("XCURSOR_SIZE").ok().as_deref());

        tracing::debug!(
            requested = requested.as_str(),
            origin = origin.as_str(),
            "cursor: resolved the startup cursor source"
        );
        Self::with_env_inputs(requested, explicit, xcursor_theme, size)
    }

    /// Testable core of [`Self::from_env`] — takes the already-read
    /// environment values so no test has to touch process-global state, and
    /// keeps them so [`Self::set_source`] can re-resolve later.
    pub fn with_env_inputs(
        requested: CursorSource,
        explicit_theme: Option<String>,
        xcursor_theme: Option<String>,
        size: u32,
    ) -> Self {
        let (source, theme_name, theme) =
            load_theme(requested, explicit_theme.as_deref(), xcursor_theme.as_deref(), size);
        Self {
            source,
            requested,
            theme_name,
            size,
            explicit_theme,
            xcursor_theme,
            theme,
            cache: HashMap::new(),
            fallback: None,
            degraded_logged: false,
        }
    }

    /// Convenience for callers (and tests) that have already resolved a theme
    /// NAME and want it used verbatim. The name is stored as the explicit
    /// override, which is exactly its semantics — an explicitly-named theme
    /// keeps winning across a later [`Self::set_source`], same as
    /// `DUDUCLAW_COMP_CURSOR_THEME` does across a restart.
    pub fn new(source: CursorSource, wanted_theme: String, size: u32) -> Self {
        Self::with_env_inputs(source, Some(wanted_theme), None, size)
    }

    /// Which source is actually in effect (already resolved through the
    /// brand→system fail-safe). Exposed for the startup log and tests.
    pub fn source(&self) -> CursorSource {
        self.source
    }

    /// CUR-2: which source was ASKED for, before the brand→system fail-safe.
    pub fn requested(&self) -> CursorSource {
        self.requested
    }

    /// The theme name actually in effect. Exposed for the startup log.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// CUR-2: switch artwork source **without restarting the compositor**.
    ///
    /// Returns `true` when something actually changed (so the caller knows
    /// whether a repaint is owed). Re-requesting the source already in effect
    /// is a no-op — notably it does NOT re-probe the filesystem, so a
    /// settings UI that writes the current value on every render costs
    /// nothing.
    ///
    /// The three things a switch has to do, and why each is necessary:
    ///
    /// 1. **Re-resolve and reload the theme.** The theme NAME is a function
    ///    of the source (`resolve_theme_name`), so the source alone is not
    ///    enough state to change.
    /// 2. **Drop the per-icon cache.** It is keyed by icon name only — it has
    ///    no theme dimension — so a stale entry would keep the OLD theme's
    ///    image on screen for every icon already drawn this session. This is
    ///    the one line that would silently half-work if forgotten: the
    ///    pointer would change only for shapes nothing had hovered yet.
    /// 3. **Re-arm the degraded warning.** A switch INTO a missing theme has
    ///    to log its own reason; the previous run's "already warned" flag
    ///    must not suppress it.
    ///
    /// What it deliberately does NOT do is touch
    /// [`super::CursorState::status`]. A client-provided cursor surface
    /// (`CursorImageStatus::Surface`) is drawn by the client, not from any
    /// theme, so it keeps its own image until that client next changes it —
    /// there is nothing here that could honestly override it.
    pub fn set_source(&mut self, requested: CursorSource) -> bool {
        if self.requested == requested {
            return false;
        }
        let (source, theme_name, theme) = load_theme(
            requested,
            self.explicit_theme.as_deref(),
            self.xcursor_theme.as_deref(),
            self.size,
        );
        self.requested = requested;
        self.source = source;
        self.theme_name = theme_name;
        self.theme = theme;
        self.cache.clear();
        self.degraded_logged = false;
        true
    }

    /// The cursor image for `icon`, loading and caching it on first use.
    ///
    /// Always returns something: theme miss → the theme's own `default`
    /// cursor → the built-in arrow. A named cursor a theme happens not to
    /// carry (plenty of themes lack e.g. `all-scroll`) must not blank the
    /// pointer.
    pub fn cursor_for(&mut self, icon: CursorIcon) -> LoadedCursor {
        let key = icon.name();
        if !self.cache.contains_key(key) {
            let loaded = self.load(icon);
            if loaded.is_none() {
                tracing::debug!(
                    icon = key,
                    theme = %self.theme_name,
                    "cursor: theme has no image for this icon — using the default cursor"
                );
            }
            self.cache.insert(key, loaded);
        }

        // Cloned out (rather than returned by reference) so the `None` arm
        // below can take `&mut self` for the lazy fallback build. The clone
        // is an `Arc` bump — see `LoadedCursor`'s doc.
        if let Some(hit) = self.cache.get(key).and_then(|slot| slot.clone()) {
            return hit;
        }

        // `Default` is the one icon a theme is effectively guaranteed to
        // have; try it before giving up on the theme entirely. Guarded
        // against infinite recursion by only recursing for non-`Default`
        // icons.
        if icon != CursorIcon::Default {
            return self.cursor_for(CursorIcon::Default);
        }

        self.fallback_cursor()
    }

    /// The asset-free built-in arrow (see [`super::fallback`]).
    pub fn fallback_cursor(&mut self) -> LoadedCursor {
        if self.fallback.is_none() {
            if !self.degraded_logged {
                self.degraded_logged = true;
                tracing::warn!(
                    theme = %self.theme_name,
                    size = self.size,
                    "cursor: falling back to the built-in outlined arrow — the configured \
                     XCursor theme provided no usable image"
                );
            }
            self.fallback = Some(LoadedCursor {
                buffer: fallback::build_buffer(self.size),
                hotspot: Point::from((0, 0)),
            });
        }
        self.fallback
            .clone()
            .expect("fallback cursor was just populated")
    }

    fn load(&self, icon: CursorIcon) -> Option<LoadedCursor> {
        // `name()` first, then the freedesktop aliases (`text` ↔ `xterm`,
        // `pointer` ↔ `hand2`, …) — themes disagree about which spelling
        // they ship, and `cursor-icon` already curates that alias list.
        let mut names: Vec<&str> = Vec::with_capacity(1 + icon.alt_names().len());
        names.push(icon.name());
        names.extend(icon.alt_names().iter().copied());

        for name in names {
            let Some(path) = self.theme.load_icon(name) else {
                continue;
            };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        path = %path.display(),
                        "cursor: failed to read theme cursor file"
                    );
                    continue;
                }
            };
            let Some(images) = xcursor::parser::parse_xcursor(&bytes) else {
                tracing::debug!(path = %path.display(), "cursor: unparseable XCursor file");
                continue;
            };
            if let Some(loaded) = build_from_images(&images, self.size) {
                return Some(loaded);
            }
        }
        None
    }
}

/// Resolves a theme name from `(requested source, explicit override,
/// XCURSOR_THEME)`, loads it, and applies the brand→system fail-safe.
///
/// Returns `(effective source, theme name in effect, loaded theme)`.
///
/// Split out of `CursorThemeStore::new` by CUR-2 so that startup and a live
/// [`CursorThemeStore::set_source`] go through the SAME code — a switch that
/// resolved theme names by a second, subtly different rule than boot did
/// would be a bug factory (the brand fail-safe in particular).
fn load_theme(
    requested: CursorSource,
    explicit_theme: Option<&str>,
    xcursor_theme: Option<&str>,
    size: u32,
) -> (CursorSource, String, xcursor::CursorTheme) {
    let wanted = source::resolve_theme_name(requested, explicit_theme, xcursor_theme);
    let theme = xcursor::CursorTheme::load(&wanted);
    // Probing for the one icon every theme is required to have is the
    // cheapest honest answer to "did we actually find a theme?" —
    // `CursorTheme::load` itself succeeds even for a name that matches no
    // directory on disk.
    let usable = theme.load_icon(CursorIcon::Default.name()).is_some();

    if usable {
        tracing::info!(
            source = requested.as_str(),
            theme = %wanted,
            size,
            "cursor: XCursor theme loaded"
        );
        return (requested, wanted, theme);
    }

    // Brand artwork is opt-in (see `source.rs`'s module doc) — asking for it
    // on a machine where the theme package is not installed must land on
    // normal system cursors, not on the asset-free fallback.
    if requested == CursorSource::Brand {
        let system = source::resolve_theme_name(CursorSource::System, None, xcursor_theme);
        let system_theme = xcursor::CursorTheme::load(&system);
        if system_theme.load_icon(CursorIcon::Default.name()).is_some() {
            tracing::warn!(
                requested_theme = %wanted,
                fell_back_to = %system,
                "cursor: brand cursor theme is not installed — falling back to the system \
                 theme (set {}=system to silence this)",
                source::CURSOR_SOURCE_ENV
            );
            return (CursorSource::System, system, system_theme);
        }
    }

    tracing::warn!(
        source = requested.as_str(),
        theme = %wanted,
        size,
        "cursor: no XCursor theme found (looked for '{}' on XCURSOR_PATH / the default \
         icon search path) — drawing the built-in outlined arrow instead. Install a cursor \
         theme (e.g. the adwaita-icon-theme package) or set {} to a theme that exists.",
        wanted,
        source::CURSOR_THEME_ENV
    );
    (requested, wanted, theme)
}

/// Picks the best image out of a parsed XCursor file and wraps it.
///
/// Split out as a free function so the size-selection rule — the one piece of
/// real logic here — is unit-testable without a filesystem or a renderer.
fn build_from_images(images: &[xcursor::parser::Image], size: u32) -> Option<LoadedCursor> {
    let img = pick_image(images, size)?;
    // `Fourcc::Argb8888` despite the field being *named* `pixels_rgba` —
    // this is a real trap in the `xcursor` crate, checked against sources
    // rather than assumed:
    //
    // * `xcursor`'s parser copies the pixel block out of the file verbatim
    //   into `pixels_rgba` (`parser.rs`'s `parse_img`: `take_bytes`, no
    //   reordering), and its own doc comment concedes "(or, in the order of
    //   the file)".
    // * libXcursor writes each pixel as an ARGB32 word through
    //   `_XcursorWriteUInt`, which emits it little-endian — so the bytes on
    //   disk are B, G, R, A.
    // * That is exactly DRM/wl_shm `ARGB8888` ("[31:0] A:R:G:B, native
    //   endian"). `wayland-cursor` — the client-side consumer of this very
    //   crate, used by winit/SCTK — writes `pixels_rgba` straight into an
    //   shm buffer declared `Format::Argb8888` (`wayland-cursor/src/lib.rs`
    //   lines 367/384), which is the battle-tested confirmation.
    //
    // The sibling field `pixels_argb` is NOT the answer either: it is
    // derived assuming the input was RGBA, so it comes out A,B,G,R. Passing
    // `Abgr8888` here (which reads bytes as R,G,B,A) would silently swap red
    // and blue — invisible on the grey/black cursors most themes ship, and
    // glaring on a coloured one.
    let buffer = MemoryRenderBuffer::from_slice(
        &img.pixels_rgba,
        Fourcc::Argb8888,
        (img.width as i32, img.height as i32),
        1,
        Transform::Normal,
        None,
    );
    Some(LoadedCursor {
        buffer,
        hotspot: Point::from((img.xhot as i32, img.yhot as i32)),
    })
}

/// The image whose nominal size is closest to `size`; ties go to the larger
/// one (downscaling artefacts beat upscaled mush), and among images sharing
/// that nominal size the **first** is taken — that is frame 0 of an animated
/// cursor. See this module's "honest limitations".
fn pick_image(images: &[xcursor::parser::Image], size: u32) -> Option<&xcursor::parser::Image> {
    let nominal = images
        .iter()
        .map(|i| i.size)
        .min_by_key(|s| (s.abs_diff(size), u32::MAX - *s))?;
    images
        .iter()
        .find(|i| i.size == nominal)
        .filter(|i| i.width > 0 && i.height > 0)
        .filter(|i| i.pixels_rgba.len() == (i.width as usize) * (i.height as usize) * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(size: u32, w: u32, h: u32, xhot: u32, yhot: u32) -> xcursor::parser::Image {
        xcursor::parser::Image {
            size,
            width: w,
            height: h,
            xhot,
            yhot,
            delay: 0,
            pixels_rgba: vec![0u8; (w * h * 4) as usize],
            pixels_argb: vec![0u8; (w * h * 4) as usize],
        }
    }

    #[test]
    fn picks_the_exact_size_when_present() {
        let images = vec![img(16, 16, 16, 1, 1), img(24, 24, 24, 2, 2), img(48, 48, 48, 3, 3)];
        assert_eq!(pick_image(&images, 24).unwrap().size, 24);
        assert_eq!(pick_image(&images, 48).unwrap().size, 48);
    }

    #[test]
    fn picks_the_nearest_size_when_the_exact_one_is_missing() {
        let images = vec![img(16, 16, 16, 1, 1), img(48, 48, 48, 3, 3)];
        assert_eq!(pick_image(&images, 20).unwrap().size, 16);
        assert_eq!(pick_image(&images, 40).unwrap().size, 48);
    }

    #[test]
    fn ties_go_to_the_larger_size() {
        // 32 is equidistant from 24 and 40; downscaling looks better than
        // upscaling, so the larger image wins.
        let images = vec![img(24, 24, 24, 1, 1), img(40, 40, 40, 2, 2)];
        assert_eq!(pick_image(&images, 32).unwrap().size, 40);
    }

    #[test]
    fn animated_cursor_yields_its_first_frame() {
        // Three frames of one animated 24px cursor: all share `size`, so the
        // selection must be stable and land on frame 0 (xhot 1 marks it).
        let images = vec![img(24, 24, 24, 1, 1), img(24, 24, 24, 9, 9), img(24, 24, 24, 8, 8)];
        assert_eq!(pick_image(&images, 24).unwrap().xhot, 1);
    }

    #[test]
    fn empty_or_truncated_images_are_refused_rather_than_panicking() {
        assert!(pick_image(&[], 24).is_none());

        let mut truncated = img(24, 24, 24, 1, 1);
        truncated.pixels_rgba.truncate(10);
        assert!(
            pick_image(&[truncated], 24).is_none(),
            "a file claiming 24x24 but carrying 10 bytes must be refused, not uploaded"
        );

        let zero = img(24, 0, 0, 0, 0);
        assert!(pick_image(&[zero], 24).is_none());
    }

    #[test]
    fn build_from_images_keeps_the_hotspot() {
        let images = vec![img(24, 24, 24, 7, 3)];
        let loaded = build_from_images(&images, 24).expect("should build");
        assert_eq!(loaded.hotspot, Point::<i32, Logical>::from((7, 3)));
    }

    #[test]
    fn a_missing_theme_still_yields_a_usable_cursor() {
        // The container/CI case: no /usr/share/icons at all. `cursor_for`
        // must never return "nothing to draw".
        let mut store = CursorThemeStore::new(
            CursorSource::System,
            "duduclaw-no-such-theme-cur1".to_string(),
            24,
        );
        let c = store.cursor_for(CursorIcon::Text);
        assert_eq!(c.hotspot, Point::<i32, Logical>::from((0, 0)));
        // Second call must hit the cached fallback, not rebuild it.
        let again = store.cursor_for(CursorIcon::Pointer);
        assert_eq!(again.hotspot, c.hotspot);
    }

    // ── CUR-2: live source switching ─────────────────────────────────────

    #[test]
    fn set_source_switches_the_theme_name_without_a_restart() {
        // No explicit override and no XCURSOR_THEME, so the theme name is a
        // pure function of the source: Adwaita for system, DuDuClaw for
        // brand. Neither needs to exist on the test machine — this asserts
        // the RESOLUTION, which is what a switch has to get right; whether
        // artwork is then found is `load_theme`'s already-tested fail-safe.
        let mut store =
            CursorThemeStore::with_env_inputs(CursorSource::System, None, None, 24);
        assert_eq!(store.requested(), CursorSource::System);
        assert_eq!(store.theme_name(), source::DEFAULT_THEME_NAME);

        assert!(store.set_source(CursorSource::Brand), "a real change reports true");
        assert_eq!(store.requested(), CursorSource::Brand);
        // `source()` may have fallen back to System (no brand theme in a bare
        // container) but the REQUEST is what a settings UI shows.
        assert!(
            store.theme_name() == source::BRAND_THEME_NAME
                || store.source() == CursorSource::System,
            "either the brand theme loaded, or the documented fail-safe ran"
        );

        assert!(store.set_source(CursorSource::System));
        assert_eq!(store.requested(), CursorSource::System);
        assert_eq!(store.theme_name(), source::DEFAULT_THEME_NAME);
    }

    #[test]
    fn set_source_to_the_current_source_is_a_no_op() {
        let mut store =
            CursorThemeStore::with_env_inputs(CursorSource::System, None, None, 24);
        assert!(
            !store.set_source(CursorSource::System),
            "re-requesting the live source must not report a change (a settings UI \
             re-asserting its own state must not cost a theme reload or a repaint)"
        );
    }

    #[test]
    fn set_source_drops_the_per_icon_cache() {
        // The cache is keyed by icon name only — it carries no theme
        // dimension — so a switch that forgot to clear it would keep drawing
        // the OLD theme's image for every icon already touched this session,
        // and only the untouched shapes would change. That failure mode is
        // invisible in a screenshot of one cursor, so it is asserted here.
        let mut store = CursorThemeStore::with_env_inputs(
            CursorSource::System,
            Some("duduclaw-no-such-theme-cur2-a".to_string()),
            None,
            24,
        );
        let _ = store.cursor_for(CursorIcon::Default);
        let _ = store.cursor_for(CursorIcon::Grab);
        assert_eq!(store.cache.len(), 2, "both lookups should have been cached");

        assert!(store.set_source(CursorSource::Brand));
        assert!(store.cache.is_empty(), "a source switch must invalidate every cached image");
    }

    #[test]
    fn an_explicit_theme_override_keeps_winning_across_a_switch() {
        // Same rule the env var has at startup (`resolve_theme_name`
        // priority 1), applied at switch time: an operator who named a theme
        // outright did not ask for the brand toggle to move it.
        let mut store = CursorThemeStore::with_env_inputs(
            CursorSource::System,
            Some("Bibata".to_string()),
            Some("Breeze".to_string()),
            24,
        );
        assert_eq!(store.theme_name(), "Bibata");
        assert!(store.set_source(CursorSource::Brand));
        assert_eq!(
            store.theme_name(),
            "Bibata",
            "an explicit theme override outranks the brand source"
        );
    }

    #[test]
    fn xcursor_theme_is_restored_when_switching_back_to_system() {
        // The failure this guards against: re-resolving on switch by reading
        // the environment again (which a running process cannot do
        // meaningfully) or by remembering only the resolved NAME would leave
        // the system side stuck on Adwaita instead of the operator's
        // XCURSOR_THEME.
        let mut store =
            CursorThemeStore::with_env_inputs(CursorSource::System, None, Some("Breeze".into()), 24);
        assert_eq!(store.theme_name(), "Breeze");
        assert!(store.set_source(CursorSource::Brand));
        assert!(store.set_source(CursorSource::System));
        assert_eq!(store.theme_name(), "Breeze");
    }
}
