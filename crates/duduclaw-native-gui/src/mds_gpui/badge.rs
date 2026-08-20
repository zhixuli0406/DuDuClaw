// Badge — MDS pill primitive (spec §4 Badge; source of truth
// `web/src/components/mds/badge.tsx`). `h-5 rounded-4xl px-2 text-xs
// font-medium`.
//
// The web catalog only defines `default/secondary/destructive/outline/
// ghost`; this facade adds `Success/Warning/Info` semantic variants per the
// S3 task brief's "semantic 色膠囊" instruction (status pills — connection
// state, task state — are the whole point of a native desktop shell having
// a Badge at all). Each new variant reuses the exact alpha-wash recipe the
// web `destructive` variant already establishes (`bg-x/10 text-x`), not an
// ad-hoc one.

use gpui::{div, prelude::*, px, Div, Hsla, Rgba, SharedString};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeVariant {
    Default,
    Secondary,
    Success,
    Warning,
    Info,
    Destructive,
    Outline,
}

struct Palette {
    bg: Rgba,
    text: Rgba,
    border: Option<Hsla>,
}

fn palette(variant: BadgeVariant) -> Palette {
    match variant {
        BadgeVariant::Default => Palette {
            bg: theme::alpha(theme::PRIMARY, 1.0),
            text: theme::alpha(theme::PRIMARY_FOREGROUND, 1.0),
            border: None,
        },
        BadgeVariant::Secondary => Palette {
            bg: theme::alpha(theme::SECONDARY, 1.0),
            text: theme::alpha(theme::SECONDARY_FOREGROUND, 1.0),
            border: None,
        },
        BadgeVariant::Success => Palette {
            bg: theme::alpha(theme::SUCCESS, 0.12),
            text: theme::alpha(theme::SUCCESS, 1.0),
            border: None,
        },
        BadgeVariant::Warning => Palette {
            bg: theme::alpha(theme::WARNING, 0.12),
            text: theme::alpha(theme::WARNING, 1.0),
            border: None,
        },
        BadgeVariant::Info => Palette {
            bg: theme::alpha(theme::INFO, 0.12),
            text: theme::alpha(theme::INFO, 1.0),
            border: None,
        },
        BadgeVariant::Destructive => Palette {
            bg: theme::alpha(theme::DESTRUCTIVE, 0.10),
            text: theme::alpha(theme::DESTRUCTIVE, 1.0),
            border: None,
        },
        BadgeVariant::Outline => Palette {
            bg: theme::alpha(theme::SURFACE, 0.0),
            text: theme::alpha(theme::FOREGROUND, 1.0),
            border: Some(theme::border()),
        },
    }
}

pub fn badge(label: impl Into<SharedString>, variant: BadgeVariant) -> Div {
    let Palette { bg, text, border } = palette(variant);
    let mut el = div()
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_4XL))
        .bg(bg)
        .text_color(text)
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into());
    if let Some(border_color) = border {
        el = el.border_1().border_color(border_color);
    }
    el
}
