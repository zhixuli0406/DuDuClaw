//! WM-2: the close button's ✕, drawn rather than typeset.
//!
//! The task brief says "✕ 手繪線條即可，不需字型", and there is a real reason
//! beyond convenience: `U+2715 MULTIPLICATION X` is not in Inter, `U+00D7` is
//! the wrong weight, and the shapes that *are* available vary by face — so a
//! font-drawn cross would look different depending on which face happened to
//! answer, which is precisely the kind of drift a 12 px affordance cannot
//! absorb. Two antialiased diagonals are deterministic.
//!
//! The maths is one line per diagonal: the distance from a point to the line
//! `y = x` is `|x − y| / √2`, and to `x + y = n` it is `|x + y − n| / √2`.
//! Coverage is a linear ramp over one pixel around `thickness / 2`, which is
//! the standard cheap analytic antialiasing for a straight edge.

use super::text::RasterizedText;

/// Stroke thickness of the ✕, in pixels.
const STROKE: f32 = 1.5;

/// Rasterises an antialiased ✕ of `size`×`size` pixels in `color`.
///
/// Output shares [`RasterizedText`]'s contract: RGBA8, byte order R,G,B,A,
/// **premultiplied** alpha — so it goes through exactly the same
/// `MemoryRenderBuffer` path as the title text and the themed cursors, rather
/// than needing a second upload mechanism.
pub fn rasterize(size: u32, color: [u8; 3]) -> RasterizedText {
    let n = size.max(1);
    let last = (n - 1) as f32;
    let half = STROKE / 2.0;
    let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;

    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32, y as f32);
            let d1 = (fx - fy).abs() * inv_sqrt2;
            let d2 = (fx + fy - last).abs() * inv_sqrt2;
            let d = d1.min(d2);
            // 1px-wide linear ramp centred on the stroke edge.
            let a = (half + 0.5 - d).clamp(0.0, 1.0);
            rgba.push((color[0] as f32 * a).round() as u8);
            rgba.push((color[1] as f32 * a).round() as u8);
            rgba.push((color[2] as f32 * a).round() as u8);
            rgba.push((a * 255.0).round() as u8);
        }
    }

    RasterizedText {
        width: n,
        height: n,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(r: &RasterizedText, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    }

    #[test]
    fn the_buffer_is_square_and_fully_populated() {
        let r = rasterize(12, [0, 0, 0]);
        assert_eq!((r.width, r.height), (12, 12));
        assert_eq!(r.rgba.len(), (12 * 12 * 4) as usize);
    }

    #[test]
    fn both_diagonals_are_inked_and_the_edges_are_not() {
        let r = rasterize(13, [0xff, 0xff, 0xff]);
        // Corners are on the diagonals -> opaque.
        for (x, y) in [(0, 0), (12, 12), (12, 0), (0, 12)] {
            assert!(pixel(&r, x, y)[3] > 200, "corner ({x},{y}) should be inked");
        }
        // Mid-edge points are as far from both diagonals as it gets.
        for (x, y) in [(6, 0), (0, 6), (12, 6), (6, 12)] {
            assert_eq!(pixel(&r, x, y)[3], 0, "edge midpoint ({x},{y}) should be clear");
        }
    }

    #[test]
    fn the_centre_is_the_crossing_point_and_is_solid() {
        let r = rasterize(13, [0, 0, 0]);
        assert!(pixel(&r, 6, 6)[3] > 200);
    }

    #[test]
    fn the_mark_is_symmetric_under_both_reflections() {
        let n = 13u32;
        let r = rasterize(n, [0x11, 0x22, 0x33]);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(
                    pixel(&r, x, y),
                    pixel(&r, n - 1 - x, y),
                    "horizontal mirror at ({x},{y})"
                );
                assert_eq!(
                    pixel(&r, x, y),
                    pixel(&r, x, n - 1 - y),
                    "vertical mirror at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn pixels_are_premultiplied_like_every_other_buffer_in_this_crate() {
        let r = rasterize(16, [0xff, 0x00, 0x00]);
        for p in r.rgba.chunks_exact(4) {
            if p[3] == 0 {
                assert_eq!(&p[0..3], &[0, 0, 0]);
            }
            assert!(p[0] as u16 <= p[3] as u16 + 1, "red channel exceeds alpha: {p:?}");
        }
    }

    #[test]
    fn a_degenerate_size_still_produces_one_pixel() {
        let r = rasterize(0, [0, 0, 0]);
        assert_eq!((r.width, r.height), (1, 1));
        assert_eq!(r.rgba.len(), 4);
    }
}
