// Skeleton — MDS loading placeholder (spec §4 Sidebar 系列; source of truth
// `web/src/components/mds/skeleton.tsx`, `animate-pulse rounded-md bg-muted`).
//
// The S3 task brief explicitly permits a static (non-animated) placeholder
// ("簡單透明度脈動或靜態即可,動畫非必須") — this implementation takes that
// option and renders a fixed 65% opacity `SURFACE_HOVER` block rather than
// wiring a real pulse animation (gpui does have an animation primitive,
// `crates/gpui/src/elements/animation.rs`, that a future pass could reach
// for). Documented here as a deliberate, brief-sanctioned scope cut, not a
// silent omission.

use gpui::{div, prelude::*, px, Div, Pixels};

use crate::theme;

pub fn skeleton(width: Pixels, height: Pixels) -> Div {
    div()
        .w(width)
        .h(height)
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::SURFACE_HOVER, 0.65))
}
