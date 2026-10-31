use super::{
    app::{App, Selection},
    text::SafeText,
};
use crate::{sanitize::TerminalText, settings::Settings, strings};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
};
pub(super) fn style(stale: bool) -> Style {
    Style::default().fg(if stale { Color::DarkGray } else { Color::White })
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
            style: style(stale),
        },
        area,
    );
}
pub(super) fn panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    settings: &Settings,
    stale: bool,
) -> Rect {
    if area.width < 2 || area.height < 2 {
        return Rect::default();
    }
    let buf = frame.buffer_mut();
    for x in area.x..area.right() {
        buf[(x, area.y)].set_symbol("-").set_style(style(stale));
        buf[(x, area.bottom() - 1)]
            .set_symbol("-")
            .set_style(style(stale));
    }
    for y in area.y..area.bottom() {
        buf[(area.x, y)].set_symbol("|").set_style(style(stale));
        buf[(area.right() - 1, y)]
            .set_symbol("|")
            .set_style(style(stale));
    }
    for (x, y) in [
        (area.x, area.y),
        (area.right() - 1, area.y),
        (area.x, area.bottom() - 1),
        (area.right() - 1, area.bottom() - 1),
    ] {
        buf[(x, y)].set_symbol("+");
    }
    text(
        frame,
        Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1),
        TerminalText::new(title.into()),
        settings,
        stale,
        false,
    );
    Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2)
}
pub(super) struct Row {
    pub selection: Selection,
    pub text: TerminalText,
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
                    style(stale).bg(Color::DarkGray).fg(if stale {
                        Color::Gray
                    } else {
                        Color::Yellow
                    })
                } else {
                    style(stale)
                },
            },
            Rect::new(area.x, area.y + y as u16, area.width, 1),
        );
    }
}
pub(super) fn title(label: &str, count: Option<usize>) -> String {
    format!(
        "{label} ({}){}",
        count
            .map(|n| n.to_string())
            .unwrap_or_else(|| strings::UNKNOWN_COUNT.into()),
        if count.is_none() {
            format!(" {}", strings::STALE)
        } else {
            String::new()
        }
    )
}
