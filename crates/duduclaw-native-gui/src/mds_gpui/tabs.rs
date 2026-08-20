// Tabs — MDS tab strip, `variant="line"` (spec §4 Tabs; source of truth
// `web/src/components/mds/tabs.tsx`). Per the S3 task brief ("下劃線式,選中
// BRAND"): an underline bar, selected label + underline colored `BRAND` —
// deliberately NOT the web catalog's `line` variant's own recipe (which
// colors the selected label/underline `foreground`, not `brand`) or its
// `default` filled-pill variant; the brief names one specific look.
//
// Facade shape: a `TabItem` carries its own fully-formed click handler
// (built by the caller via `cx.listener(...)`, same as every other
// interactive row in this crate — see `screens/shell.rs`'s `nav_row`) so
// this module never needs to know about `RootView` or any other concrete
// `Entity` type.

use gpui::{div, prelude::*, px, App, ClickEvent, Div, SharedString, Window};

use crate::theme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub struct TabItem {
    pub id: &'static str,
    pub label: SharedString,
    pub on_click: ClickHandler,
}

impl TabItem {
    pub fn new(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self { id, label: label.into(), on_click: Box::new(on_click) }
    }
}

pub fn tabs(items: Vec<TabItem>, active_id: &str) -> Div {
    let mut row = Vec::with_capacity(items.len());
    for item in items {
        let selected = item.id == active_id;
        let on_click = item.on_click;
        row.push(
            div()
                .id(item.id)
                .cursor_pointer()
                .pb_1p5()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if selected {
                    theme::alpha(theme::BRAND, 1.0)
                } else {
                    theme::alpha(theme::MUTED_FOREGROUND, 1.0)
                })
                .when(!selected, |el| {
                    el.hover(|style| style.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                })
                // Border is present on every row (not just the selected one)
                // so the underline's 2px doesn't shift unselected rows'
                // height relative to the selected one — only its color
                // toggles.
                .border_b_2()
                .border_color(if selected {
                    theme::alpha(theme::BRAND, 1.0)
                } else {
                    theme::alpha(theme::BRAND, 0.0)
                })
                .child(item.label)
                .on_click(move |ev, window, cx| on_click(ev, window, cx)),
        );
    }
    div()
        .flex()
        .items_center()
        .gap_4()
        .border_b_1()
        .border_color(theme::border())
        .children(row)
}
