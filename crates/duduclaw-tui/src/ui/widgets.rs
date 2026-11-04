use super::{
    app::{App, Selection},
    text::SafeText,
};
use crate::{sanitize::TerminalText, settings::Settings, strings};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
};
pub(super) fn style(settings: &Settings, stale: bool) -> Style {
    if stale { settings.colors.muted() } else { settings.colors.base() }
}
pub(super) fn text(
    frame: &mut Frame,
    area: Rect,
    value: TerminalText,
    settings: &Settings,
    stale: bool,
    wrap: bool,
) {
    frame.render_widget(
        SafeText {
            text: &value,
            width: settings.ambiguous_width,
            visible: false,
            wrap,
            scroll: 0,
            style: style(settings, stale),
        },
        area,
    );
}
pub(super) fn panel_focus(frame: &mut Frame, area: Rect, title: &str, settings: &Settings, stale: bool, focused: bool) -> Rect {
    use ratatui::{symbols::border, widgets::BorderType};
    let set = if settings.glyphs == crate::settings::Glyphs::Unicode {
        BorderType::Rounded.to_border_set()
    } else { border::Set { top_left: "+", top_right: "+", bottom_left: "+", bottom_right: "+",
        vertical_left: "|", vertical_right: "|", horizontal_top: "-", horizontal_bottom: "-" } };
    let rail = super::text::columns(set.vertical_left, settings.ambiguous_width) as u16;
    if area.width < rail * 2 || area.height < 2 { return Rect::default(); }
    frame.buffer_mut().set_style(area, settings.colors.base());
    let border_style = settings.colors.border_style(focused);
    let mut put = |x, y, value: &str, width| {
        frame.render_widget(SafeText { text: &TerminalText::new(value.into()), width: settings.ambiguous_width,
            visible: false, wrap: false, scroll: 0, style: border_style }, Rect::new(x, y, width, 1));
    };
    let horizontal = super::text::columns(set.horizontal_top, settings.ambiguous_width).max(1) as u16;
    for x in (area.x + rail..area.right() - rail).step_by(usize::from(horizontal)) {
        let width = (area.right() - rail - x).min(horizontal);
        put(x, area.y, set.horizontal_top, width);
        put(x, area.bottom() - 1, set.horizontal_bottom, width);
    }
    for y in area.y + 1..area.bottom() - 1 {
        put(area.x, y, set.vertical_left, rail);
        put(area.right() - rail, y, set.vertical_right, rail);
    }
    for (x,y,symbol) in [(area.x,area.y,set.top_left), (area.right()-rail,area.y,set.top_right),
        (area.x,area.bottom()-1,set.bottom_left), (area.right()-rail,area.bottom()-1,set.bottom_right)] {
        put(x, y, symbol, rail);
    }
    if title.is_empty() {
        return Rect::new(area.x + rail, area.y + 1, area.width - rail * 2, area.height - 2);
    }
    let safe_title = TerminalText::new(title.into());
    let available = area.width.saturating_sub(rail * 4);
    let title_columns = super::text::columns(&safe_title.plain().replace('\n', " "), settings.ambiguous_width);
    let clear_columns = (title_columns.saturating_add(1)).div_ceil(usize::from(horizontal)) * usize::from(horizontal);
    let title_area = Rect::new(area.x + rail * 2, area.y, available.min(clear_columns.min(usize::from(u16::MAX)) as u16), 1);
    // Clear both halves of any wide border glyph before painting mixed-width text.
    for x in title_area.x..title_area.right() {
        frame.buffer_mut()[(x, title_area.y)].reset();
        frame.buffer_mut()[(x, title_area.y)].set_style(border_style);
    }
    frame.render_widget(SafeText { text: &safe_title, width: settings.ambiguous_width,
        visible: false, wrap: false, scroll: 0,
        style: if focused { border_style } else { style(settings, stale) } }, title_area);
    Rect::new(area.x + rail, area.y + 1, area.width - rail * 2, area.height - 2)
}

/// A persistent content region has a title and whitespace, never another ring.
pub(super) fn section(frame: &mut Frame, area: Rect, title: &str, settings: &Settings, stale: bool, focused: bool) -> Rect {
    if area.is_empty() || title.is_empty() { return area; }
    let title_style = if stale { settings.colors.muted() } else if focused {
        settings.colors.foreground(settings.colors.secondary)
    } else { settings.colors.muted() };
    frame.render_widget(SafeText { text: &TerminalText::new(title.into()), width: settings.ambiguous_width,
        visible: false, wrap: false, scroll: 0, style: title_style }, Rect::new(area.x, area.y, area.width, 1));
    Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1))
}

pub(super) fn separator(frame: &mut Frame, area: Rect, settings: &Settings) {
    let glyph = if settings.glyphs == crate::settings::Glyphs::Unicode { "─" } else { "-" };
    let width = super::text::columns(glyph, settings.ambiguous_width).max(1);
    frame.render_widget(SafeText { text: &TerminalText::new(glyph.repeat(usize::from(area.width) / width)),
        width: settings.ambiguous_width, visible: false, wrap: false, scroll: 0,
        style: settings.colors.border_style(false) }, area);
}

pub(super) struct Row {
    pub selection: Selection,
    pub text: TerminalText,
    pub status: Option<super::app::EmployeeStatus>,
}
pub(super) fn rows(
    frame: &mut Frame,
    area: Rect,
    rows: Vec<Row>,
    app: &App,
    settings: &Settings,
    stale: bool,
) {
    let selected = app
        .selected
        .as_ref()
        .and_then(|s| rows.iter().position(|r| &r.selection == s));
    let offset = selected
        .map(|i| (i + 1).saturating_sub(usize::from(area.height)))
        .unwrap_or(0);
    for (y, row) in rows
        .into_iter()
        .skip(offset)
        .take(usize::from(area.height))
        .enumerate()
    {
        let selected = app.selected.as_ref() == Some(&row.selection);
        let value = TerminalText::new(format!(
            "{} {}",
            if selected { ">" } else { " " },
            row.text.plain().replace('\n', " ")
        ));
        frame.render_widget(
            SafeText {
                text: &value,
                width: settings.ambiguous_width,
                visible: false,
                wrap: false,
                scroll: 0,
                style: if selected {
                    let selected = settings.colors.selected();
                    if stale { settings.colors.readable_style(selected.fg(settings.colors.muted).remove_modifier(Modifier::BOLD)) } else { selected }
                } else {
                    style(settings, stale)
                },
            },
            Rect::new(area.x, area.y + y as u16, area.width, 1),
        );
        if let Some(status) = row.status {
            let glyph = status.glyph(settings.glyphs);
            let glyph_width = super::text::columns(glyph, settings.ambiguous_width) as u16;
            if area.width >= 2 + glyph_width {
                let mut glyph_style = if selected { settings.colors.selected() } else { settings.colors.base() };
                glyph_style = glyph_style.fg(if stale { settings.colors.muted } else { status.color(&settings.colors) });
                if !stale { glyph_style = glyph_style.add_modifier(Modifier::BOLD); }
                frame.render_widget(SafeText { text: &TerminalText::new(glyph.into()), width: settings.ambiguous_width,
                    visible: false, wrap: false, scroll: 0, style: settings.colors.readable_style(glyph_style) },
                    Rect::new(area.x + 2, area.y + y as u16, glyph_width, 1));
            }
        }
    }
}
pub(super) fn title(label: &str, count: Option<usize>) -> String {
    match count {
        Some(n) => format!("{label} ({n})"),
        None => format!("{label} {}", strings::STALE),
    }
}

/// Style fixed key hints after width-aware rendering; never parse gateway text as markup.
pub(super) fn key_hints(frame: &mut Frame, area: Rect, value: TerminalText, settings: &Settings) {
    use unicode_segmentation::UnicodeSegmentation;
    let safe = value.plain();
    frame.render_widget(SafeText { text: &value, width: settings.ambiguous_width, visible: false,
        wrap: true, scroll: 0, style: settings.colors.muted() }, area);
    let mut x = area.x;
    let mut y = area.y;
    for g in safe.graphemes(true) {
        let n = super::text::columns(g, settings.ambiguous_width) as u16;
        if n == 0 || n > area.width { continue; }
        if g == "\n" || x + n > area.right() { x = area.x; y += 1; }
        if y >= area.bottom() { break; }
        if g != " " && (g.is_ascii() || g.chars().all(|c| "↑↓←→".contains(c))) {
            frame.buffer_mut()[(x,y)].set_style(settings.colors.foreground(settings.colors.accent));
        }
        x += n;
    }
}
