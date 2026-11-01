//! Confirmation for reversible controls, separate from all gateway content.
use crate::{controls::Control, sanitize::TerminalText, strings};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};
#[derive(Clone)]
pub enum Source {
    Employee(String),
    Schedule(String),
    Pane(usize),
}
#[derive(Clone)]
pub struct ControlCard {
    pub control: Control,
    pub source: Source,
    pub fingerprint: serde_json::Value,
    pub code: String,
    pub input: String,
    pub scroll: usize,
    pub blocked: bool,
    pub editing: bool,
    opened: Instant,
    bottom: bool,
    max_scroll: usize,
    page: usize,
}
impl ControlCard {
    pub fn new(
        control: Control,
        source: Source,
        fingerprint: serde_json::Value,
        code: String,
        now: Instant,
    ) -> Self {
        let editing = matches!(control, Control::Steer { .. });
        Self {
            control,
            source,
            fingerprint,
            code,
            input: String::new(),
            scroll: 0,
            blocked: false,
            editing,
            opened: now,
            bottom: false,
            max_scroll: 0,
            page: 1,
        }
    }
    pub fn ready(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.opened) >= Duration::from_secs(1)
    }
    pub fn body(&self) -> TerminalText {
        TerminalText::new(format!(
            "{}\n{}\n{}",
            self.control.label(),
            strings::CONTROL_SCOPE,
            serde_json::to_string_pretty(&self.control.request().1).unwrap_or_default()
        ))
    }
    pub fn viewport(&mut self, lines: usize, height: usize) {
        self.page = height.max(1);
        self.max_scroll = lines.saturating_sub(height);
        self.scroll = self.scroll.min(self.max_scroll);
        self.bottom = height > 0 && self.scroll >= self.max_scroll;
        if !self.bottom {
            self.input.clear();
        }
    }
    pub fn resize(&mut self) {
        self.bottom = false;
        self.input.clear();
    }
    pub fn paste(&mut self, text: &str) {
        if self.editing
            && let Control::Steer { body, .. } = &mut self.control
        {
            let remaining = 4000usize.saturating_sub(body.chars().count());
            body.extend(
                text.chars()
                    .filter(|c| !crate::sanitize::forbidden(*c))
                    .take(remaining),
            );
        }
    }
    pub fn key(&mut self, key: KeyEvent, now: Instant, fresh: bool) -> Option<Control> {
        if key.kind != KeyEventKind::Press
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            || now.saturating_duration_since(self.opened) < Duration::from_secs(1)
        {
            return None;
        }
        if self.editing {
            match key.code {
                KeyCode::Char(c) => self.paste(&c.to_string()),
                KeyCode::Backspace => {
                    if let Control::Steer { body, .. } = &mut self.control {
                        body.pop();
                    }
                }
                KeyCode::Enter if matches!(&self.control, Control::Steer { body, .. } if !body.trim().is_empty()) =>
                {
                    self.editing = false;
                    self.opened = now;
                    self.resize();
                    self.scroll = 0;
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(self.page).min(self.max_scroll)
            }
            KeyCode::PageUp | KeyCode::Up => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::Char(c) if c.is_ascii_digit() && self.bottom && fresh && !self.blocked => {
                if self.input.len() < 2 {
                    self.input.push(c);
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter if self.bottom && fresh && !self.blocked && self.input == self.code => {
                return Some(self.control.clone());
            }
            _ => {
                self.input.clear();
            }
        }
        None
    }
    pub fn hints(&self, fresh: bool) -> TerminalText {
        TerminalText::new(if self.blocked {
            strings::CARD_REOPEN.into()
        } else if !fresh {
            strings::STALE.into()
        } else if self.editing {
            strings::STEER_EDIT.into()
        } else if !self.bottom {
            strings::CONTROL_SCROLL.into()
        } else {
            format!(
                "{} {} [{}]\n{}",
                strings::CARD_CODE,
                self.code,
                self.input,
                strings::CONTROL_CONFIRM
            )
        })
    }
}
pub(super) fn draw(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    card: &mut ControlCard,
    settings: &crate::settings::Settings,
    fresh: bool,
) {
    use super::{
        text::{SafeText, wrapped_lines},
        widgets::{panel, style},
    };
    use ratatui::layout::Rect;
    frame.render_widget(ratatui::widgets::Clear, area);
    let inner = panel(frame, area, strings::CARD_TITLE, settings, !fresh);
    // Match the existing decision card's double rail, distinct from data panes.
    if !inner.is_empty() {
        for x in area.x..area.right() {
            frame.buffer_mut()[(x, area.y)].set_symbol("=");
            frame.buffer_mut()[(x, area.bottom() - 1)].set_symbol("=");
        }
        super::widgets::text(
            frame,
            Rect::new(area.x + 2, area.y, area.width.saturating_sub(4), 1),
            TerminalText::new(strings::CARD_TITLE.into()),
            settings,
            !fresh,
            false,
        );
    }
    let height = inner.height.saturating_sub(5);
    let body = card.body();
    card.viewport(
        wrapped_lines(&body, inner.width, settings.ambiguous_width),
        height as usize,
    );
    frame.render_widget(
        SafeText {
            text: &body,
            width: settings.ambiguous_width,
            visible: true,
            wrap: true,
            scroll: card.scroll,
            style: style(!fresh),
        },
        Rect::new(inner.x, inner.y, inner.width, height),
    );
    let hint = card.hints(fresh);
    frame.render_widget(
        SafeText {
            text: &hint,
            width: settings.ambiguous_width,
            visible: true,
            wrap: true,
            scroll: 0,
            style: style(!fresh),
        },
        Rect::new(
            inner.x,
            inner.y + height,
            inner.width,
            inner.height - height,
        ),
    );
}
