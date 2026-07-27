//! Local pixel-art conversion — zero external API (WP-P5-lite).
//!
//! Turns a background-removed RGBA cutout into a crisp, retro "petdex / Codex
//! Pets" pixel sprite entirely offline:
//!
//! 1. **Downscale** to a small canonical width (~64px), aspect-preserved, with
//!    nearest-neighbour sampling — the source of the chunky pixel grid.
//! 2. **Alpha threshold** — pixel art has hard edges, so partially-transparent
//!    fringe pixels snap to fully-opaque or fully-transparent.
//! 3. **Colour quantise** the opaque pixels to a small palette (median-cut) so
//!    the result reads as a limited-palette sprite, not a shrunk photo.
//! 4. **Upscale** back to the original size with nearest-neighbour for a crisp
//!    standalone PNG (the procedural single-image path / studio preview).
//!
//! The small quantised image ([`quantize_pixel_art`]) is the canonical low-res
//! sprite the spritesheet baker (`sprite_bake`) works from; [`pixelate_rgba`] is
//! the upscaled-to-original convenience wrapper.

use image::{imageops::FilterType, Rgba, RgbaImage};

/// Default canonical pixel-grid width (columns) for a pet sprite.
pub const DEFAULT_PIXEL_WIDTH: u32 = 64;
/// Default palette size (distinct colours) for quantisation.
pub const DEFAULT_PALETTE_SIZE: usize = 16;
/// Alpha at/above which a downscaled pixel is treated as fully opaque.
const ALPHA_CUTOFF: u8 = 128;

/// Produce the canonical small pixel-art image: downscale to `target_width`
/// (aspect-preserved, nearest), hard-threshold alpha, and quantise the opaque
/// colours to at most `palette_size` entries.
///
/// `target_width` and `palette_size` are clamped to sane minimums (≥1 / ≥2).
pub fn quantize_pixel_art(src: &RgbaImage, target_width: u32, palette_size: usize) -> RgbaImage {
    let target_width = target_width.max(1);
    let palette_size = palette_size.max(2);
    let (w, h) = (src.width().max(1), src.height().max(1));

    // Aspect-preserved target height (round to nearest, never zero).
    let target_height =
        (((target_width as u64) * (h as u64) + (w as u64) / 2) / (w as u64)).max(1) as u32;

    // 1. Nearest-neighbour downscale → chunky grid.
    let mut small = image::imageops::resize(src, target_width, target_height, FilterType::Nearest);

    // 2. Hard alpha edges.
    for px in small.pixels_mut() {
        px[3] = if px[3] >= ALPHA_CUTOFF { 255 } else { 0 };
    }

    // 3. Quantise opaque colours.
    let opaque: Vec<[u8; 3]> = small
        .pixels()
        .filter(|p| p[3] == 255)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    let palette = median_cut(&opaque, palette_size);
    if !palette.is_empty() {
        for px in small.pixels_mut() {
            if px[3] == 255 {
                let c = nearest_color(&palette, [px[0], px[1], px[2]]);
                *px = Rgba([c[0], c[1], c[2], 255]);
            }
        }
    }
    small
}

/// Full pixel-art transform: [`quantize_pixel_art`] then nearest-neighbour
/// upscale back to the source dimensions for a crisp standalone PNG.
pub fn pixelate_rgba(src: &RgbaImage, target_width: u32, palette_size: usize) -> RgbaImage {
    let small = quantize_pixel_art(src, target_width, palette_size);
    image::imageops::resize(
        &small,
        src.width().max(1),
        src.height().max(1),
        FilterType::Nearest,
    )
}

/// Count the distinct opaque RGB colours in an image (test / diagnostic helper).
pub fn count_distinct_colors(img: &RgbaImage) -> usize {
    let mut set = std::collections::HashSet::new();
    for p in img.pixels() {
        if p[3] == 255 {
            set.insert([p[0], p[1], p[2]]);
        }
    }
    set.len()
}

// ── Median-cut quantisation ──────────────────────────────────────────────────

/// A colour box: the slice of pixels it covers plus its channel ranges.
struct ColorBox {
    colors: Vec<[u8; 3]>,
}

impl ColorBox {
    /// The channel (0=R,1=G,2=B) with the widest value range, and that range.
    fn widest_channel(&self) -> (usize, u16) {
        let mut widest = (0usize, 0u16);
        for ch in 0..3 {
            let mut lo = u8::MAX;
            let mut hi = u8::MIN;
            for c in &self.colors {
                lo = lo.min(c[ch]);
                hi = hi.max(c[ch]);
            }
            let range = (hi - lo) as u16;
            if range >= widest.1 {
                widest = (ch, range);
            }
        }
        widest
    }

    /// Average colour of the box (its palette representative).
    fn average(&self) -> [u8; 3] {
        if self.colors.is_empty() {
            return [0, 0, 0];
        }
        let mut sum = [0u64; 3];
        for c in &self.colors {
            for ch in 0..3 {
                sum[ch] += c[ch] as u64;
            }
        }
        let n = self.colors.len() as u64;
        [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
    }
}

/// Median-cut: split the colour space into at most `max_colors` boxes and return
/// each box's average colour. Deterministic (stable sort on the widest channel).
fn median_cut(colors: &[[u8; 3]], max_colors: usize) -> Vec<[u8; 3]> {
    if colors.is_empty() {
        return Vec::new();
    }
    let mut boxes = vec![ColorBox {
        colors: colors.to_vec(),
    }];

    while boxes.len() < max_colors {
        // Pick the splittable box with the widest single-channel range.
        let target = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.colors.len() > 1)
            .max_by_key(|(_, b)| b.widest_channel().1)
            .map(|(i, _)| i);
        let Some(idx) = target else { break };

        let mut b = boxes.swap_remove(idx);
        let (ch, range) = b.widest_channel();
        if range == 0 {
            // All colours identical — nothing to split; put it back and stop.
            boxes.push(b);
            break;
        }
        // Sort along the widest channel and split at the median.
        b.colors.sort_by_key(|c| c[ch]);
        let mid = b.colors.len() / 2;
        let hi = b.colors.split_off(mid);
        boxes.push(ColorBox { colors: b.colors });
        boxes.push(ColorBox { colors: hi });
    }

    boxes.iter().map(ColorBox::average).collect()
}

/// Nearest palette colour to `target` by squared Euclidean distance in RGB.
fn nearest_color(palette: &[[u8; 3]], target: [u8; 3]) -> [u8; 3] {
    let mut best = palette[0];
    let mut best_d = u32::MAX;
    for c in palette {
        let dr = c[0] as i32 - target[0] as i32;
        let dg = c[1] as i32 - target[1] as i32;
        let db = c[2] as i32 - target[2] as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = *c;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient image so quantisation has many source colours to collapse.
    fn gradient(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(1)) as u8;
                let g = (y * 255 / h.max(1)) as u8;
                let b = ((x + y) * 255 / (w + h).max(1)) as u8;
                img.put_pixel(x, y, Rgba([r, g, b, 255]));
            }
        }
        img
    }

    #[test]
    fn downscales_to_target_width_preserving_aspect() {
        let src = gradient(200, 100);
        let small = quantize_pixel_art(&src, 64, 16);
        assert_eq!(small.width(), 64);
        // 100 * 64 / 200 = 32.
        assert_eq!(small.height(), 32);
    }

    #[test]
    fn quantises_to_at_most_palette_size_colours() {
        let src = gradient(120, 120);
        // The source gradient has hundreds of distinct colours.
        assert!(count_distinct_colors(&src) > 16);
        let small = quantize_pixel_art(&src, 64, 16);
        assert!(
            count_distinct_colors(&small) <= 16,
            "expected ≤16 colours, got {}",
            count_distinct_colors(&small)
        );
        assert!(
            count_distinct_colors(&small) >= 2,
            "should keep some variety"
        );
    }

    #[test]
    fn hard_thresholds_alpha() {
        let mut src = RgbaImage::new(8, 8);
        for (i, px) in src.pixels_mut().enumerate() {
            let a = (i * 4).min(255) as u8;
            *px = Rgba([200, 100, 50, a]);
        }
        let small = quantize_pixel_art(&src, 8, 8);
        for px in small.pixels() {
            assert!(px[3] == 0 || px[3] == 255, "alpha must be hard: {}", px[3]);
        }
    }

    #[test]
    fn pixelate_upscales_back_to_source_size() {
        let src = gradient(96, 64);
        let out = pixelate_rgba(&src, 48, 12);
        assert_eq!(out.width(), 96);
        assert_eq!(out.height(), 64);
        // Upscaled from a ≤12-colour small image ⇒ still ≤12 opaque colours.
        assert!(count_distinct_colors(&out) <= 12);
    }

    #[test]
    fn deterministic_output() {
        let src = gradient(80, 80);
        let a = quantize_pixel_art(&src, 40, 16);
        let b = quantize_pixel_art(&src, 40, 16);
        assert_eq!(a.into_raw(), b.into_raw());
    }

    #[test]
    fn fewer_unique_colours_than_palette_is_fine() {
        let mut src = RgbaImage::new(10, 10);
        for px in src.pixels_mut() {
            *px = Rgba([10, 20, 30, 255]);
        }
        let small = quantize_pixel_art(&src, 8, 16);
        assert_eq!(count_distinct_colors(&small), 1);
    }
}
