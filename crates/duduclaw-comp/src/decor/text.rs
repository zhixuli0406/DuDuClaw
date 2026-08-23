//! WM-2: rasterising the `xdg_toplevel.title` into an RGBA buffer.
//!
//! # Why the fonts are embedded rather than loaded from the system
//!
//! Same reasoning `cursor/fallback.rs` recorded for the built-in arrow: this
//! compositor runs both on the appliance image (which has fonts) and in a bare
//! `rust:bookworm` build container (which has none, and is where every
//! `BUILD.md` verification round happens). A title bar whose text silently
//! disappears in one of those two environments is a bug that only ever shows
//! up on the machine you are not looking at. Two static TTFs are
//! `include_bytes!`-embedded, so the same pixels come out everywhere.
//!
//! The two faces come from `crates/duduclaw-native-gui/assets/fonts/static/`
//! — the shell's own weight-500 pair, so a window title matches the shell's
//! chrome instead of being a second typographic voice on the same screen.
//!
//! # Why `ab_glyph` and not a shaping engine
//!
//! A title bar is left-to-right, single-line, no ligature requirements, no
//! bidi, no Indic/Arabic reordering. Glyph-per-char with horizontal advances
//! is exactly correct for Latin and CJK, which is what this product's users
//! actually see. Pulling in HarfBuzz (a C dependency) or a full text stack for
//! a 32 px strip would be a large amount of new surface for no visible
//! difference. Recorded as a **known limitation** rather than hidden: a title
//! in Arabic or Devanagari will render unshaped.
//!
//! # CJK fallback
//!
//! Per character: if Inter has no glyph for it (`glyph_id(..).0 == 0`, i.e.
//! `.notdef`), the same character is looked up in Noto Sans TC. Both the
//! outline **and** the advance come from whichever face won, which is the part
//! that is easy to get wrong — measuring in one font and drawing in another
//! produces text that overlaps itself.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};

/// Hard cap on how many characters of a client-supplied title are even
/// measured. A `set_title` with a megabyte of text is a denial-of-service on
/// the render path, not a title; the visible area holds a few dozen glyphs at
/// most, so this cap can never truncate something that would have been drawn.
const MAX_TITLE_CHARS: usize = 256;

/// The character appended when a title does not fit.
const ELLIPSIS: char = '…';

/// One rasterised run of text: RGBA8, premultiplied alpha, byte order R,G,B,A
/// (i.e. `Fourcc::Abgr8888` on a little-endian machine — the same choice, for
/// the same reason, as `cursor/fallback.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterizedText {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The two embedded faces.
pub struct FontSet {
    primary: FontRef<'static>,
    fallback: FontRef<'static>,
}

const INTER_500: &[u8] = include_bytes!("../../assets/fonts/Inter-500.ttf");
const NOTO_SANS_TC_500: &[u8] = include_bytes!("../../assets/fonts/NotoSansTC-500.ttf");

impl FontSet {
    /// Parses both embedded faces.
    ///
    /// Returns `None` only if an embedded asset fails to parse, which would be
    /// a build-time mistake (a truncated vendored file), not a runtime
    /// condition. Callers degrade to "no title text" rather than panicking —
    /// an untitled title bar is still a usable, draggable, closable window.
    pub fn load() -> Option<Self> {
        let primary = FontRef::try_from_slice(INTER_500).ok()?;
        let fallback = FontRef::try_from_slice(NOTO_SANS_TC_500).ok()?;
        Some(Self { primary, fallback })
    }

    /// Which face draws `ch`, and its glyph id **in that face**.
    fn pick(&self, ch: char) -> (&FontRef<'static>, GlyphId) {
        let gid = self.primary.glyph_id(ch);
        if gid.0 != 0 {
            return (&self.primary, gid);
        }
        let gid = self.fallback.glyph_id(ch);
        if gid.0 != 0 {
            return (&self.fallback, gid);
        }
        // Neither face has it: draw the primary's `.notdef` box, which is a
        // visible "this character is missing" rather than a silent gap.
        (&self.primary, self.primary.glyph_id(ch))
    }

    /// Horizontal advance of one character at `px`, in pixels.
    fn advance(&self, ch: char, px: f32) -> f32 {
        let (font, gid) = self.pick(ch);
        font.as_scaled(PxScale::from(px)).h_advance(gid)
    }

    /// Total advance width of a string at `px`.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        text.chars()
            .take(MAX_TITLE_CHARS)
            .map(|ch| self.advance(ch, px))
            .sum()
    }

    /// Line box height and baseline offset at `px`, both rounded up to whole
    /// pixels. Taken from the PRIMARY face only: mixing two faces' vertical
    /// metrics per line would make the baseline jump whenever a CJK character
    /// appears.
    fn line_metrics(&self, px: f32) -> (u32, f32) {
        let s = self.primary.as_scaled(PxScale::from(px));
        let ascent = s.ascent();
        let descent = s.descent(); // negative
        let height = (ascent - descent).ceil().max(1.0);
        (height as u32, ascent.ceil())
    }

    /// Shortens `text` so it fits in `max_width` pixels, appending `…` when it
    /// had to cut.
    ///
    /// Cutting happens on `char` boundaries by construction (the input is
    /// iterated as `chars`, never sliced by byte index) — this crate's
    /// coding-convention 1 forbids raw byte slicing precisely because every
    /// CJK title would panic on it.
    pub fn truncate_to_width(&self, text: &str, px: f32, max_width: i32) -> String {
        if max_width <= 0 {
            return String::new();
        }
        let max = max_width as f32;
        let chars: Vec<char> = text.chars().take(MAX_TITLE_CHARS).collect();

        let mut total = 0.0f32;
        let mut fits_whole = true;
        for &ch in &chars {
            total += self.advance(ch, px);
            if total > max {
                fits_whole = false;
                break;
            }
        }
        if fits_whole {
            return chars.into_iter().collect();
        }

        let ellipsis_w = self.advance(ELLIPSIS, px);
        if ellipsis_w > max {
            // Not even one ellipsis fits: draw nothing rather than a stray dot.
            return String::new();
        }
        let budget = max - ellipsis_w;
        let mut used = 0.0f32;
        let mut kept = String::new();
        for &ch in &chars {
            let w = self.advance(ch, px);
            if used + w > budget {
                break;
            }
            used += w;
            kept.push(ch);
        }
        kept.push(ELLIPSIS);
        kept
    }

    /// Rasterises `text` at `px` in `color`, truncating to `max_width` first.
    ///
    /// `None` when there is nothing to draw (empty text, no room, or a run
    /// that produced a zero-area box) — never an empty buffer, because a
    /// zero-sized `MemoryRenderBuffer` is a renderer error rather than an
    /// invisible element.
    pub fn rasterize(
        &self,
        text: &str,
        px: f32,
        color: [u8; 3],
        max_width: i32,
    ) -> Option<RasterizedText> {
        let text = self.truncate_to_width(text, px, max_width);
        if text.is_empty() {
            return None;
        }

        let (height, baseline) = self.line_metrics(px);
        let scale = PxScale::from(px);

        // Lay out first (position + face per glyph), then draw. Two passes so
        // the buffer is allocated exactly once, at the real width.
        let mut pen = 0.0f32;
        let mut placed: Vec<(&FontRef<'static>, GlyphId, f32)> = Vec::new();
        for ch in text.chars() {
            let (font, gid) = self.pick(ch);
            placed.push((font, gid, pen));
            pen += font.as_scaled(scale).h_advance(gid);
        }

        let width = pen.ceil().max(1.0) as u32;
        let width = width.min(max_width.max(1) as u32);
        if width == 0 || height == 0 {
            return None;
        }

        // Coverage accumulator, one f32 per pixel. Kept separate from the RGBA
        // output so overlapping glyphs (kerned pairs, CJK marks) combine as
        // `max` instead of double-darkening.
        let mut coverage = vec![0.0f32; (width * height) as usize];
        for (font, gid, x) in placed {
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(x, baseline));
            let Some(outlined) = font.outline_glyph(glyph) else {
                continue; // whitespace has no outline
            };
            let bounds = outlined.px_bounds();
            let ox = bounds.min.x;
            let oy = bounds.min.y;
            outlined.draw(|gx, gy, c| {
                let px_x = ox + gx as f32;
                let px_y = oy + gy as f32;
                if px_x < 0.0 || px_y < 0.0 {
                    return;
                }
                let (ix, iy) = (px_x as u32, px_y as u32);
                if ix >= width || iy >= height {
                    return;
                }
                let slot = &mut coverage[(iy * width + ix) as usize];
                if c > *slot {
                    *slot = c;
                }
            });
        }

        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for c in &coverage {
            let a = c.clamp(0.0, 1.0);
            // Premultiplied: the Wayland / `MemoryRenderBuffer` convention.
            rgba.push((color[0] as f32 * a).round() as u8);
            rgba.push((color[1] as f32 * a).round() as u8);
            rgba.push((color[2] as f32 * a).round() as u8);
            rgba.push((a * 255.0).round() as u8);
        }

        Some(RasterizedText {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> FontSet {
        FontSet::load().expect("both embedded fonts must parse")
    }

    #[test]
    fn both_embedded_fonts_parse() {
        // Guards against a vendored file being truncated or replaced by an
        // LFS pointer — the failure mode would otherwise be "titles are blank
        // on the appliance" with no other symptom.
        let f = fonts();
        assert_ne!(f.primary.glyph_id('A').0, 0, "Inter must have Latin 'A'");
        assert_ne!(f.fallback.glyph_id('視').0, 0, "Noto Sans TC must have 視");
    }

    #[test]
    fn latin_comes_from_inter_and_cjk_from_noto() {
        let f = fonts();
        // Inter has no CJK, so the fallback must be what answers.
        assert_eq!(f.primary.glyph_id('視').0, 0, "Inter is not expected to carry CJK");
        let (_, gid) = f.pick('視');
        assert_ne!(gid.0, 0, "the CJK character must resolve through the fallback face");
    }

    #[test]
    fn a_short_title_is_returned_unchanged() {
        let f = fonts();
        let out = f.truncate_to_width("Chromium", 13.0, 400);
        assert_eq!(out, "Chromium");
    }

    #[test]
    fn a_long_title_is_truncated_with_an_ellipsis() {
        let f = fonts();
        let long = "A very long window title that will certainly not fit in eighty pixels";
        let out = f.truncate_to_width(long, 13.0, 80);
        assert!(out.ends_with('…'), "got {out:?}");
        assert!(out.chars().count() < long.chars().count());
        assert!(
            f.measure(&out, 13.0) <= 80.0,
            "truncated text must fit: {out:?} measured {}",
            f.measure(&out, 13.0)
        );
    }

    #[test]
    fn a_cjk_title_truncates_on_character_boundaries() {
        // The whole point of coding-convention 1: never slice a CJK string by
        // byte index. If this ever regresses to `&s[..n]` it panics here.
        let f = fonts();
        let title = "嘟嘟數位工作室－值班機視窗管理測試視窗標題";
        let out = f.truncate_to_width(title, 13.0, 60);
        assert!(out.ends_with('…'));
        // Every retained character must be a whole character from the input.
        for ch in out.chars().filter(|c| *c != '…') {
            assert!(title.contains(ch), "{ch:?} is not a character of the original");
        }
        assert!(f.measure(&out, 13.0) <= 60.0);
    }

    #[test]
    fn a_mixed_latin_and_cjk_title_measures_with_both_faces() {
        let f = fonts();
        let mixed = "Chromium — 嘟嘟值班機";
        // Non-zero, finite, and wider than the Latin part alone.
        let all = f.measure(mixed, 13.0);
        let latin = f.measure("Chromium — ", 13.0);
        assert!(all.is_finite() && all > latin, "{all} vs {latin}");
    }

    #[test]
    fn a_zero_or_negative_width_yields_no_text_at_all() {
        let f = fonts();
        assert_eq!(f.truncate_to_width("anything", 13.0, 0), "");
        assert_eq!(f.truncate_to_width("anything", 13.0, -20), "");
        assert!(f.rasterize("anything", 13.0, [0, 0, 0], 0).is_none());
    }

    #[test]
    fn a_width_too_small_even_for_the_ellipsis_yields_nothing() {
        let f = fonts();
        // One pixel cannot hold an ellipsis.
        assert_eq!(f.truncate_to_width("a very long title indeed", 13.0, 1), "");
    }

    #[test]
    fn an_absurdly_long_title_is_capped_before_measuring() {
        let f = fonts();
        let huge: String = "x".repeat(100_000);
        let out = f.truncate_to_width(&huge, 13.0, 200);
        assert!(out.chars().count() <= MAX_TITLE_CHARS + 1);
        // And measuring it is bounded too.
        assert!(f.measure(&huge, 13.0).is_finite());
    }

    #[test]
    fn rasterize_produces_a_full_rgba_buffer_with_real_ink() {
        let f = fonts();
        let r = f
            .rasterize("Chromium", 13.0, [0x1c, 0x19, 0x17], 400)
            .expect("visible text");
        assert_eq!(r.rgba.len(), (r.width * r.height * 4) as usize);
        assert!(r.width > 0 && r.height > 0);
        let opaque_pixels = r.rgba.chunks_exact(4).filter(|p| p[3] > 128).count();
        assert!(opaque_pixels > 20, "expected real glyph coverage, got {opaque_pixels}");
    }

    #[test]
    fn rasterize_never_exceeds_the_width_it_was_given() {
        let f = fonts();
        for max in [10, 40, 120, 400] {
            if let Some(r) = f.rasterize("A long enough window title", 13.0, [0, 0, 0], max) {
                assert!(
                    r.width <= max as u32,
                    "max {max} produced a {}px buffer",
                    r.width
                );
            }
        }
    }

    #[test]
    fn rasterized_pixels_are_premultiplied() {
        // A transparent pixel must be fully zero, and a fully covered pixel
        // must carry the requested colour verbatim — that pair is what makes
        // the buffer safe to hand to `Fourcc::Abgr8888` without a
        // premultiply pass (same invariant `cursor/fallback.rs` relies on).
        let f = fonts();
        let r = f.rasterize("H", 40.0, [0xff, 0x00, 0x00], 400).expect("ink");
        let mut saw_clear = false;
        let mut saw_solid = false;
        for p in r.rgba.chunks_exact(4) {
            if p[3] == 0 {
                assert_eq!(&p[0..3], &[0, 0, 0], "transparent pixel is not premultiplied");
                saw_clear = true;
            }
            if p[3] == 255 {
                assert_eq!(&p[0..3], &[0xff, 0x00, 0x00]);
                saw_solid = true;
            }
        }
        assert!(saw_clear && saw_solid, "clear={saw_clear} solid={saw_solid}");
    }

    #[test]
    fn whitespace_only_titles_produce_a_buffer_without_panicking() {
        let f = fonts();
        // Space has an advance but no outline — the layout loop must skip the
        // missing outline rather than unwrap it.
        let r = f.rasterize("   ", 13.0, [0, 0, 0], 400);
        if let Some(r) = r {
            assert_eq!(r.rgba.iter().filter(|b| **b != 0).count(), 0);
        }
    }

    #[test]
    fn an_empty_title_rasterizes_to_nothing() {
        let f = fonts();
        assert!(f.rasterize("", 13.0, [0, 0, 0], 400).is_none());
    }
}
