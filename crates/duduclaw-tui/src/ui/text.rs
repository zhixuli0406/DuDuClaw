//! Width-aware safe text widget. ASCII borders are independent of ambiguous width.
use crate::{sanitize::TerminalText, settings::AmbiguousWidth};
use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::Rect,
    style::Style,
    widgets::Widget,
};
use std::num::NonZeroU16;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub struct SafeText<'a> {
    pub text: &'a TerminalText,
    pub width: AmbiguousWidth,
    pub visible: bool,
    pub wrap: bool,
    pub scroll: usize,
    pub style: Style,
}
impl Widget for SafeText<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let safe = if self.visible {
            self.text.visible()
        } else {
            self.text.plain()
        };
        let mut x = area.x;
        let mut line = 0usize;
        for g in safe.graphemes(true) {
            if g == "\n" {
                if self.wrap {
                    x = area.x;
                    line += 1;
                } else {
                    break;
                }
                continue;
            }
            let width = match self.width {
                AmbiguousWidth::Narrow => g.width(),
                AmbiguousWidth::Wide => g.width_cjk(),
            };
            // Standalone combining characters cannot overwrite a preceding border.
            let Some(width) = u16::try_from(width).ok().and_then(NonZeroU16::new) else {
                continue;
            };
            let n = width.get();
            if n > area.width {
                continue;
            }
            if x.saturating_add(n) > area.right() {
                if !self.wrap {
                    break;
                }
                x = area.x;
                line += 1;
            }
            if line >= self.scroll + usize::from(area.height) {
                break;
            }
            if line >= self.scroll {
                let y = area.y + (line - self.scroll) as u16;
                buf[(x, y)]
                    .set_symbol(g)
                    .set_style(self.style)
                    .set_diff_option(CellDiffOption::ForcedWidth(width));
                for dx in 1..n {
                    buf[(x + dx, y)].reset();
                }
            }
            x += n;
        }
    }
}
