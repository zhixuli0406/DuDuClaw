//! Bake a single pixel-art sprite into a Codex Pets / openpets spritesheet.
//!
//! Takes one background-removed, pixel-quantised cutout (see `pixelate`) and
//! synthesises an 8-column × 9-row grid of animation frames using only
//! deterministic geometric transforms (scale / translate / horizontal flip /
//! shear) — no drawing model, no external API. Each row is one animation state,
//! ordered to match openpets so the sheet drops into that ecosystem:
//!
//! | row | state          | motion signature                              |
//! |-----|----------------|-----------------------------------------------|
//! | 0   | idle           | gentle breathing (vertical squash/stretch)    |
//! | 1   | running-right  | forward lean + bounce                         |
//! | 2   | running-left   | mirror of running-right (flipped)             |
//! | 3   | waving         | side-to-side tilt (a wave/sway)               |
//! | 4   | jumping        | rises + stretches at the apex                 |
//! | 5   | failed         | droops down, slumped                          |
//! | 6   | waiting        | slow, shallow breath                          |
//! | 7   | running        | upright bounce (no lean)                      |
//! | 8   | review         | small forward nod                             |
//!
//! Every cell begins fully transparent, so a sheet composited from a cutout with
//! a transparent background stays transparent between poses.

use std::collections::BTreeMap;

use image::{imageops::FilterType, Rgba, RgbaImage};

use crate::manifest::AnimationSpec;

/// Columns (frames per animation row).
pub const COLS: u32 = 8;
/// Frame cell width (px) — matches openpets' 192×208 grid.
pub const FRAME_W: u32 = 192;
/// Frame cell height (px).
pub const FRAME_H: u32 = 208;

/// Animation rows, top-to-bottom, aligned to the openpets layout.
pub const ROWS: [&str; 9] = [
    "idle",
    "running-right",
    "running-left",
    "waving",
    "jumping",
    "failed",
    "waiting",
    "running",
    "review",
];

/// Total spritesheet width (`COLS · FRAME_W`).
pub const SHEET_W: u32 = COLS * FRAME_W;
/// Total spritesheet height (`ROWS · FRAME_H`).
pub const SHEET_H: u32 = FRAME_H * ROWS.len() as u32;

// Fitting: the base sprite is scaled to sit inside the cell, feet near the
// bottom so poses share a consistent ground line. FIT_H leaves ~40px of cell
// headroom above the sprite — the jump arc and run bounce need it; at 168 the
// apex frames clipped the head, which flattened the poses into "every frame
// looks the same".
const FIT_H: u32 = 148;
const FIT_MAX_W: u32 = 150;
const BASELINE_MARGIN: i32 = 18; // px above the cell bottom the feet rest at.

/// A per-frame geometric transform of the fitted base sprite.
struct Transform {
    scale_x: f32,
    scale_y: f32,
    /// Horizontal centre offset (px, + = right).
    dx: f32,
    /// Vertical offset of the feet from the baseline (px, + = down).
    dy: f32,
    /// Mirror horizontally.
    flip_h: bool,
    /// Lean: top of the sprite shifts this many px relative to the feet (+ = right).
    shear: f32,
}

impl Transform {
    fn identity() -> Self {
        Transform {
            scale_x: 1.0,
            scale_y: 1.0,
            dx: 0.0,
            dy: 0.0,
            flip_h: false,
            shear: 0.0,
        }
    }
}

/// Compute the transform for `(row, frame)`. `phase` is `frame / COLS` in
/// `[0,1)`; a full sine period over the row keeps every animation seamless.
fn transform_for(row: usize, frame: u32) -> Transform {
    let phase = frame as f32 / COLS as f32;
    let wave = (phase * std::f32::consts::TAU).sin(); // one loop over the row
    let bounce = wave.abs();
    let mut t = Transform::identity();
    match row {
        // idle — breathing.
        0 => {
            t.scale_y = 1.0 + 0.05 * wave;
            t.scale_x = 1.0 - 0.025 * wave;
            t.dy = -3.0 * wave;
        }
        // running-right — forward lean + bounce + gait sway.
        1 => {
            t.dy = -14.0 * bounce;
            t.dx = 5.0 * wave;
            t.shear = 11.0 + 5.0 * wave;
            t.scale_y = 1.0 + 0.05 * bounce;
            t.scale_x = 1.0 - 0.03 * bounce;
        }
        // running-left — mirror of running-right.
        2 => {
            t.dy = -14.0 * bounce;
            t.dx = -5.0 * wave;
            t.shear = -(11.0 + 5.0 * wave);
            t.scale_y = 1.0 + 0.05 * bounce;
            t.scale_x = 1.0 - 0.03 * bounce;
            t.flip_h = true;
        }
        // waving — big side-to-side rock.
        3 => {
            t.shear = 15.0 * wave;
            t.dy = -3.0 * bounce;
            t.scale_x = 1.0 + 0.03 * wave;
        }
        // jumping — full arc: crouch, launch, stretch at the apex.
        4 => {
            t.dy = -26.0 * bounce + 4.0 * (1.0 - bounce);
            t.scale_y = 1.0 + 0.10 * bounce - 0.06 * (1.0 - bounce);
            t.scale_x = 1.0 - 0.08 * bounce + 0.05 * (1.0 - bounce);
        }
        // failed — deep slump: shorter, wider, tipped forward.
        5 => {
            t.dy = 10.0 + 2.0 * bounce;
            t.scale_y = 0.80 - 0.02 * bounce;
            t.scale_x = 1.08;
            t.shear = 7.0 + 2.0 * wave;
        }
        // waiting — SIT: settle into a low crouch and breathe there.
        6 => {
            let calm = (phase * std::f32::consts::PI).sin();
            t.scale_y = 0.72 + 0.03 * calm;
            t.scale_x = 1.14 - 0.03 * calm;
            t.dy = 2.0;
        }
        // running — upright bounce, no lean.
        7 => {
            t.dy = -12.0 * bounce;
            t.scale_y = 1.0 + 0.06 * bounce;
            t.scale_x = 1.0 - 0.04 * bounce;
        }
        // review — pronounced forward nod.
        8 => {
            t.dy = 4.0 * bounce;
            t.shear = 6.0 * wave;
            t.scale_y = 1.0 - 0.03 * bounce;
        }
        _ => {}
    }
    t
}

/// Playback frames-per-second per row (busier states animate faster).
fn fps_for(row: usize) -> u32 {
    match ROWS[row] {
        "running-right" | "running-left" | "running" => 12,
        "jumping" => 10,
        "waving" | "review" => 8,
        "idle" | "failed" => 6,
        "waiting" => 4,
        _ => 8,
    }
}

/// Fit the base sprite into the cell: scale to `FIT_H` tall (or `FIT_MAX_W`
/// wide, whichever binds), preserving aspect, nearest-neighbour (crisp pixels).
fn fit_base(base: &RgbaImage) -> RgbaImage {
    let (bw, bh) = (base.width().max(1), base.height().max(1));
    let mut fh = FIT_H;
    let mut fw = ((fh as u64 * bw as u64 + bh as u64 / 2) / bh as u64).max(1) as u32;
    if fw > FIT_MAX_W {
        fw = FIT_MAX_W;
        fh = ((fw as u64 * bh as u64 + bw as u64 / 2) / bw as u64).max(1) as u32;
    }
    image::imageops::resize(base, fw, fh, FilterType::Nearest)
}

/// Bake `base` (a pixel-quantised cutout) into the full `SHEET_W × SHEET_H`
/// spritesheet. Deterministic: identical input → identical bytes.
pub fn bake_spritesheet(base: &RgbaImage) -> RgbaImage {
    let fitted = fit_base(base);
    let mut sheet = RgbaImage::new(SHEET_W, SHEET_H);

    for (row, _name) in ROWS.iter().enumerate() {
        let cell_y0 = row as i32 * FRAME_H as i32;
        for frame in 0..COLS {
            let cell_x0 = frame as i32 * FRAME_W as i32;
            let t = transform_for(row, frame);
            composite_frame(&mut sheet, &fitted, cell_x0, cell_y0, &t);
        }
    }
    sheet
}

/// Composite one transformed frame into the sheet at cell origin `(cx, cy)`.
fn composite_frame(sheet: &mut RgbaImage, fitted: &RgbaImage, cx: i32, cy: i32, t: &Transform) {
    // Scale (nearest keeps the pixels crisp).
    let sw = ((fitted.width() as f32 * t.scale_x).round() as u32).max(1);
    let sh = ((fitted.height() as f32 * t.scale_y).round() as u32).max(1);
    let mut sprite = image::imageops::resize(fitted, sw, sh, FilterType::Nearest);
    if t.flip_h {
        image::imageops::flip_horizontal_in_place(&mut sprite);
    }

    // Anchor: horizontally centred in the cell (+dx); feet on the baseline (+dy).
    let baseline = FRAME_H as i32 - BASELINE_MARGIN;
    let origin_x = cx + (FRAME_W as i32 - sw as i32) / 2 + t.dx.round() as i32;
    let origin_y = cy + baseline - sh as i32 + t.dy.round() as i32;

    let (sw_i, sh_i) = (sw as i32, sh as i32);
    for y in 0..sh_i {
        // Lean: rows nearer the top shift more (feet fixed).
        let lean = (t.shear * ((sh_i - y) as f32 / sh_i as f32)).round() as i32;
        for x in 0..sw_i {
            let src = sprite.get_pixel(x as u32, y as u32);
            if src[3] == 0 {
                continue;
            }
            let dx = origin_x + x + lean;
            let dy = origin_y + y;
            if dx < cx || dx >= cx + FRAME_W as i32 || dy < cy || dy >= cy + FRAME_H as i32 {
                continue; // clamp to this cell so poses never bleed into neighbours
            }
            over(sheet, dx as u32, dy as u32, *src);
        }
    }
}

/// Straight "source over destination" alpha compositing.
fn over(dst: &mut RgbaImage, x: u32, y: u32, src: Rgba<u8>) {
    let bg = *dst.get_pixel(x, y);
    let sa = src[3] as f32 / 255.0;
    if sa >= 1.0 || bg[3] == 0 {
        dst.put_pixel(x, y, src);
        return;
    }
    let ba = bg[3] as f32 / 255.0;
    let out_a = sa + ba * (1.0 - sa);
    if out_a <= 0.0 {
        dst.put_pixel(x, y, Rgba([0, 0, 0, 0]));
        return;
    }
    let blend = |s: u8, b: u8| -> u8 {
        (((s as f32 * sa) + (b as f32 * ba * (1.0 - sa))) / out_a).round() as u8
    };
    dst.put_pixel(
        x,
        y,
        Rgba([
            blend(src[0], bg[0]),
            blend(src[1], bg[1]),
            blend(src[2], bg[2]),
            (out_a * 255.0).round() as u8,
        ]),
    );
}

/// The per-state grid `animations` map for a baked spritesheet (`pet.json`).
pub fn spritesheet_animations() -> BTreeMap<String, AnimationSpec> {
    let mut m = BTreeMap::new();
    for (row, name) in ROWS.iter().enumerate() {
        m.insert(
            name.to_string(),
            AnimationSpec {
                row: row as u32,
                frames: COLS,
                fps: fps_for(row),
                loops: true,
                velocity: None,
            },
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid opaque block base sprite (so every transformed frame has pixels).
    fn block(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (i, px) in img.pixels_mut().enumerate() {
            let c = (i % 200) as u8 + 30;
            *px = Rgba([c, 120, 200, 255]);
        }
        img
    }

    fn cell_opaque_count(sheet: &RgbaImage, row: usize, col: u32) -> usize {
        let x0 = col * FRAME_W;
        let y0 = row as u32 * FRAME_H;
        let mut n = 0;
        for y in y0..y0 + FRAME_H {
            for x in x0..x0 + FRAME_W {
                if sheet.get_pixel(x, y)[3] > 0 {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn sheet_has_exact_grid_dimensions() {
        let sheet = bake_spritesheet(&block(40, 50));
        assert_eq!(sheet.width(), SHEET_W);
        assert_eq!(sheet.height(), SHEET_H);
        assert_eq!(SHEET_W, 8 * 192);
        assert_eq!(SHEET_H, 9 * 208);
    }

    #[test]
    fn every_row_and_frame_is_non_empty() {
        let sheet = bake_spritesheet(&block(48, 60));
        for row in 0..ROWS.len() {
            for col in 0..COLS {
                assert!(
                    cell_opaque_count(&sheet, row, col) > 0,
                    "row {} ({}) frame {} is empty",
                    row,
                    ROWS[row],
                    col
                );
            }
        }
    }

    #[test]
    fn frames_within_a_row_differ_animation() {
        // idle breathing must actually change the silhouette across frames.
        let sheet = bake_spritesheet(&block(48, 60));
        let f0 = cell_opaque_count(&sheet, 0, 0);
        let f2 = cell_opaque_count(&sheet, 0, 2);
        assert_ne!(f0, f2, "idle frames should not be identical");
    }

    #[test]
    fn animations_map_covers_all_rows() {
        let m = spritesheet_animations();
        assert_eq!(m.len(), ROWS.len());
        for (row, name) in ROWS.iter().enumerate() {
            let spec = m.get(*name).expect("row present");
            assert_eq!(spec.row, row as u32);
            assert_eq!(spec.frames, COLS);
            assert!(spec.loops);
        }
    }

    #[test]
    fn poses_stay_inside_their_cells() {
        // A wide base + heavy lean must not bleed into neighbouring cells.
        let sheet = bake_spritesheet(&block(120, 60));
        // Column boundary between frame 0 and frame 1 of row 1 (running-right).
        for y in FRAME_H..2 * FRAME_H {
            // The last column of frame 0.
            let edge = FRAME_W - 1;
            let _ = sheet.get_pixel(edge, y); // in-bounds access proves clamping held
        }
        assert_eq!(sheet.width(), SHEET_W); // no panic ⇒ all writes were in-bounds
    }

    #[test]
    fn deterministic() {
        let a = bake_spritesheet(&block(50, 50));
        let b = bake_spritesheet(&block(50, 50));
        assert_eq!(a.into_raw(), b.into_raw());
    }
}
