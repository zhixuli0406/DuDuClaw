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
    source: CursorSource,
    theme_name: String,
    size: u32,
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
        let source = CursorSource::from_env();
        let explicit = std::env::var(source::CURSOR_THEME_ENV).ok();
        let xcursor_theme = std::env::var("XCURSOR_THEME").ok();
        let size = source::resolve_size(std::env::var("XCURSOR_SIZE").ok().as_deref());

        let wanted = source::resolve_theme_name(source, explicit.as_deref(), xcursor_theme.as_deref());
        Self::new(source, wanted, size)
    }

    /// Testable core of [`Self::from_env`] — takes the already-resolved
    /// settings so no test has to touch process-global environment state.
    pub fn new(source: CursorSource, wanted_theme: String, size: u32) -> Self {
        let theme = xcursor::CursorTheme::load(&wanted_theme);
        // Probing for the one icon every theme is required to have is the
        // cheapest honest answer to "did we actually find a theme?" —
        // `CursorTheme::load` itself succeeds even for a name that matches no
        // directory on disk.
        let usable = theme.load_icon(CursorIcon::Default.name()).is_some();

        if usable {
            tracing::info!(
                source = source.as_str(),
                theme = %wanted_theme,
                size,
                "cursor: XCursor theme loaded"
            );
            return Self::with_theme(source, wanted_theme, size, theme);
        }

        // Brand artwork is opt-in and does not exist yet (see
        // `source.rs`'s module doc) — asking for it on a machine without the
        // theme installed must land on normal system cursors, not on the
        // asset-free fallback.
        if source == CursorSource::Brand {
            let system = source::resolve_theme_name(
                CursorSource::System,
                None,
                std::env::var("XCURSOR_THEME").ok().as_deref(),
            );
            let system_theme = xcursor::CursorTheme::load(&system);
            if system_theme.load_icon(CursorIcon::Default.name()).is_some() {
                tracing::warn!(
                    requested_theme = %wanted_theme,
                    fell_back_to = %system,
                    "cursor: brand cursor theme is not installed — falling back to the system \
                     theme (set {}=system to silence this)",
                    source::CURSOR_SOURCE_ENV
                );
                return Self::with_theme(CursorSource::System, system, size, system_theme);
            }
        }

        tracing::warn!(
            source = source.as_str(),
            theme = %wanted_theme,
            size,
            "cursor: no XCursor theme found (looked for '{}' on XCURSOR_PATH / the default \
             icon search path) — drawing the built-in outlined arrow instead. Install a cursor \
             theme (e.g. the adwaita-icon-theme package) or set {} to a theme that exists.",
            wanted_theme,
            source::CURSOR_THEME_ENV
        );
        Self::with_theme(source, wanted_theme, size, theme)
    }

    fn with_theme(
        source: CursorSource,
        theme_name: String,
        size: u32,
        theme: xcursor::CursorTheme,
    ) -> Self {
        Self {
            source,
            theme_name,
            size,
            theme,
            cache: HashMap::new(),
            fallback: None,
            degraded_logged: false,
        }
    }

    /// Which source is actually in effect (already resolved through the
    /// brand→system fail-safe). Exposed for the startup log and tests.
    pub fn source(&self) -> CursorSource {
        self.source
    }

    /// The theme name actually in effect. Exposed for the startup log.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
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
}
