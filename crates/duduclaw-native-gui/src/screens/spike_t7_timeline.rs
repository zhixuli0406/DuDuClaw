// WP-gpui-spike-T7 — primitive 2: horizontally-scrolling hour-grid timeline
// lane. See `spike_t7.rs`'s module doc comment for the spike's overall
// scope/rationale/deletion note; this file is one of its three sibling
// primitive modules, not a standalone page.
//
// Feeds the /timeline risk page. Content is one `canvas()` sized to the
// FULL 24-hour width (not just the viewport) inside an `.overflow_x_scroll()`
// + `.track_scroll(...)` container — the standard gpui scroll recipe already
// used by `screens/chat/conversation_row.rs` (vertical there, horizontal
// here). Deliberately brute-force (paints every block every frame, no
// virtualization by scroll position) — the point of the primitive-4 stress
// toggle is to find out whether that brute-force approach already holds up
// at 500 blocks before spending time on a virtualized version.

use gpui::{div, prelude::*, px, Div, ScrollHandle, Stateful};

use crate::screens::spike_t7::{chart_color, pseudo_rand};
use crate::theme;

const HOURS: usize = 24;
const PX_PER_HOUR: f32 = 90.0;
const CONTENT_WIDTH: f32 = HOURS as f32 * PX_PER_HOUR;
const LANE_COUNT: usize = 3;
const LANE_HEIGHT: f32 = 46.0;
const LANE_GAP: f32 = 8.0;
const HEADER_HEIGHT: f32 = 22.0;
const CONTENT_HEIGHT: f32 = HEADER_HEIGHT + (LANE_HEIGHT + LANE_GAP) * LANE_COUNT as f32;
const VIEWPORT_HEIGHT: f32 = CONTENT_HEIGHT + 4.0;

const BASELINE_BLOCKS: usize = 100;
const STRESS_BLOCKS: usize = 500;

pub fn render(stress_mode: bool, scroll_handle: &ScrollHandle) -> Stateful<Div> {
    let block_count = if stress_mode { STRESS_BLOCKS } else { BASELINE_BLOCKS };

    div()
        .id("spike-t7-timeline-viewport")
        .w(px(860.0))
        .h(px(VIEWPORT_HEIGHT))
        .overflow_x_scroll()
        .track_scroll(scroll_handle)
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .child(
            gpui::canvas(
                move |_bounds, _window, _cx| {},
                move |bounds, _prepaint, window, cx| {
                    paint_hour_grid(bounds, window, cx);
                    paint_blocks(bounds, block_count, window);
                },
            )
            .w(px(CONTENT_WIDTH))
            .h(px(CONTENT_HEIGHT)),
        )
}

fn paint_hour_grid(bounds: gpui::Bounds<gpui::Pixels>, window: &mut gpui::Window, cx: &mut gpui::App) {
    let line_color = theme::alpha(theme::BORDER, theme::BORDER_ALPHA * 2.0);
    for hour in 0..=HOURS {
        let x = bounds.origin.x + px(hour as f32 * PX_PER_HOUR);
        let line_bounds =
            gpui::Bounds::new(gpui::point(x, bounds.origin.y + px(HEADER_HEIGHT)), gpui::size(px(1.0), px(CONTENT_HEIGHT - HEADER_HEIGHT)));
        window.paint_quad(gpui::fill(line_bounds, line_color));

        if hour < HOURS {
            let label: gpui::SharedString = format!("{hour:02}:00").into();
            let font = window.text_style().font();
            let run = gpui::TextRun {
                len: label.len(),
                font,
                color: theme::alpha(theme::MUTED_FOREGROUND, 1.0).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(label, px(theme::TEXT_XS), &[run], None);
            let label_origin = gpui::point(x + px(4.0), bounds.origin.y + px(4.0));
            if let Err(e) = shaped.paint(label_origin, px(HEADER_HEIGHT), gpui::TextAlign::Left, None, window, cx) {
                // Rendering glitch, not fatal — see `ime_input/element.rs`'s
                // identical "log and keep going" convention for a shaped-
                // line paint failure.
                eprintln!("[spike_t7_timeline] hour label paint failed: {e}");
            }
        }
    }
}

fn paint_blocks(bounds: gpui::Bounds<gpui::Pixels>, block_count: usize, window: &mut gpui::Window) {
    for i in 0..block_count {
        let seed = i as u64;
        let lane = i % LANE_COUNT;
        // Deterministic scatter: start hour anywhere in [0, HOURS), width
        // 20–70px, clamped so no block starts past the last hour column.
        let start_x = pseudo_rand(seed * 3 + 1) * (CONTENT_WIDTH - 70.0);
        let width = 20.0 + pseudo_rand(seed * 3 + 2) * 50.0;
        let lane_top = HEADER_HEIGHT + lane as f32 * (LANE_HEIGHT + LANE_GAP);

        let block_bounds = gpui::Bounds::new(
            bounds.origin + gpui::point(px(start_x), px(lane_top)),
            gpui::size(px(width), px(LANE_HEIGHT)),
        );
        window.paint_quad(gpui::quad(
            block_bounds,
            px(4.0),
            theme::alpha(chart_color(i), 0.85),
            px(1.0),
            theme::alpha(theme::SURFACE_BORDER, 1.0),
            gpui::BorderStyle::default(),
        ));
    }
}
