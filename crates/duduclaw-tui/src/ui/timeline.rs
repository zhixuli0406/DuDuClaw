//! Bounded presentation of already-received run and chat metadata.
//! This module never fetches data and never infers events from transcript text.
use super::{app::App, text::{SafeText, columns, middle_ellipsis}};
use crate::{blocks::{Block, Pane}, sanitize::TerminalText, settings::{Glyphs, Settings}, strings};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect};
use std::{collections::{HashSet, VecDeque}, hash::{Hash, Hasher}};

const MAX_ROWS: usize = 256;
const MAX_DETAIL_CHARS: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome { Done, Failed, Pending }
impl Outcome {
    fn status(status: &str) -> Self {
        match status {
            "completed" | "done" | "success" | "succeeded" | "ok" => Self::Done,
            "failed" | "error" | "cancelled" | "canceled" => Self::Failed,
            _ => Self::Pending,
        }
    }
    fn glyph(self, glyphs: Glyphs) -> &'static str {
        match (self, glyphs) {
            (Self::Done, Glyphs::Unicode) => "✓",
            (Self::Failed, Glyphs::Unicode) => "✕",
            (Self::Pending, Glyphs::Unicode) => "⋯",
            (Self::Done, Glyphs::Ascii) => "+",
            (Self::Failed, Glyphs::Ascii) => "x",
            (Self::Pending, Glyphs::Ascii) => "...",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    id: String,
    time: String,
    verb: &'static str,
    target: TerminalText,
    detail: TerminalText,
    outcome: Outcome,
    tool_scope: Option<(String, Option<usize>)>,
    turn: u64,
    waiting: bool,
    elapsed_ms: Option<u64>,
}
fn bounded(text: &str) -> TerminalText {
    TerminalText::new(text.chars().take(MAX_DETAIL_CHARS).collect())
}
impl Row {
    fn summary(&self, width: u16, settings: &Settings, selected: bool, expanded: bool) -> TerminalText {
        let glyph = match (settings.glyphs, expanded) {
            (Glyphs::Unicode, false) => "▸", (Glyphs::Unicode, true) => "▾",
            (Glyphs::Ascii, false) => ">", (Glyphs::Ascii, true) => "v",
        };
        let prefix = format!("{} {} {glyph} {} ", if selected { ">" } else { " " }, crate::human_time::format(&self.time), self.verb);
        let suffix = format!(" {}", self.outcome.glyph(settings.glyphs));
        let suffix = middle_ellipsis(&suffix, usize::from(width), settings.ambiguous_width);
        let available = usize::from(width).saturating_sub(columns(&suffix, settings.ambiguous_width));
        let prefix = middle_ellipsis(&prefix, available, settings.ambiguous_width);
        let budget = available.saturating_sub(columns(&prefix, settings.ambiguous_width));
        let target = self.target.plain().replace('\n', " ");
        // Keep the result glyph at the end even when a target is a very long CJK name.
        TerminalText::new(format!("{prefix}{}{suffix}", middle_ellipsis(&target, budget, settings.ambiguous_width)))
    }
}

#[derive(Default, Clone)]
pub(crate) struct View {
    pub(crate) focused: bool,
    selected: Option<String>,
    expanded: HashSet<String>,
    scroll: usize,
    scrolling: bool,
}
impl View {
    pub(super) fn conversation_offset(&mut self, count: usize, selected: Option<usize>, height: usize, tail_scroll: usize) -> usize {
        let max = count.saturating_sub(height);
        let offset = if self.focused && self.scrolling { self.scroll }
            else if self.focused { selected.unwrap_or(count).saturating_sub(height / 3) }
            else { max.saturating_sub(tail_scroll) };
        self.scroll = offset.min(max);
        self.scroll
    }

    fn reconcile(&mut self, rows: &[Row]) -> usize {
        self.expanded.retain(|id| rows.iter().any(|row| &row.id == id));
        let index = self.selected.as_ref().and_then(|id| rows.iter().position(|row| &row.id == id))
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        self.selected = rows.get(index).map(|row| row.id.clone());
        index
    }
    fn key(&mut self, key: KeyEvent, rows: &[Row]) -> bool {
        let index = self.reconcile(rows);
        if matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k' | 't') | KeyCode::Enter | KeyCode::Esc) { self.scrolling = false; }
        if key.code == KeyCode::Char('t') {
            self.focused = !self.focused;
            return true;
        }
        if !self.focused { return false; }
        match key.code {
            KeyCode::Esc => self.focused = false,
            KeyCode::PageDown => { self.scroll = self.scroll.saturating_add(5); self.scrolling = true; }
            KeyCode::PageUp => { self.scroll = self.scroll.saturating_sub(5); self.scrolling = true; }
            KeyCode::Up | KeyCode::Char('k') => self.selected = rows.get(index.saturating_sub(1)).map(|row| row.id.clone()),
            KeyCode::Down | KeyCode::Char('j') => self.selected = rows.get((index + 1).min(rows.len().saturating_sub(1))).map(|row| row.id.clone()),
            KeyCode::Enter => if let Some(row) = rows.get(index) && !self.expanded.remove(&row.id) { self.expanded.insert(row.id.clone()); },
            _ => return false,
        }
        true
    }
}

/// Only called after ChatState has validated the frame's room and running turn.
#[derive(Default, Clone)]
pub(crate) struct ChatActivity {
    rows: VecDeque<Row>,
    next: u64,
    turn: u64,
}
impl ChatActivity {
    pub(crate) fn turn(&self) -> u64 { self.turn }
    pub(crate) fn has_running(&self) -> bool { self.rows.iter().any(|r| r.tool_scope.is_some() && !r.waiting && r.outcome == Outcome::Pending) }
    pub(crate) fn begin_turn(&mut self) {
        self.turn = self.turn.wrapping_add(1);
        for row in &mut self.rows { row.tool_scope = None; row.waiting = false; }
    }
    pub(crate) fn record(&mut self, frame: &crate::chat::Frame) {
        let time = frame.ts.and_then(|ts| i64::try_from(ts).ok()).and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .unwrap_or_else(chrono::Utc::now).to_rfc3339();
        let tool_scope = frame.tool.as_ref().filter(|tool| frame.kind == "step" && tool.len() <= MAX_DETAIL_CHARS)
            .map(|tool| (tool.clone(), frame.depth));
        if tool_scope.is_some() && matches!(frame.phase.as_deref(), Some("waiting" | "approval" | "waiting_approval" | "start"))
            && let Some(row) = self.rows.iter_mut().rev().find(|row| row.tool_scope == tool_scope && row.outcome == Outcome::Pending
                && (frame.phase.as_deref() != Some("start") || row.waiting)) {
            row.waiting = frame.phase.as_deref() != Some("start");
            return;
        }
        if tool_scope.is_some() && frame.kind == "step" && matches!(frame.phase.as_deref(), Some("end" | "completed" | "done" | "success" | "succeeded" | "ok" | "failed" | "error" | "cancelled"))
            && let Some(row) = self.rows.iter_mut().rev().find(|row| row.tool_scope == tool_scope && row.outcome == Outcome::Pending) {
            // The wire's end boundary has no success/error field. Completion is
            // known, but its result remains unreported rather than invented.
            row.tool_scope = None;
            row.waiting = false;
            row.outcome = Outcome::status(frame.phase.as_deref().unwrap_or(""));
            row.elapsed_ms = chrono::DateTime::parse_from_rfc3339(&row.time).ok()
                .zip(frame.ts).and_then(|(start, end)| end.checked_sub(start.timestamp_millis().max(0) as u64));
            return;
        }
        if matches!(frame.kind.as_str(), "assistant_done" | "error" | "cancelled") {
            for row in &mut self.rows { row.tool_scope = None; row.waiting = false; }
        }
        let step_target = format!("{} {}", frame.tool.as_deref().unwrap_or("工具"), frame.summary.as_deref().unwrap_or(""));
        let (verb, target, detail, outcome) = match frame.kind.as_str() {
            "step" => ("執行", step_target.as_str(),
                format!("{}\n{}", frame.tool.as_deref().unwrap_or(""), frame.summary.as_deref().unwrap_or("")),
                Outcome::status(frame.phase.as_deref().unwrap_or(""))),
            "assistant_done" => ("回合", "對話", frame.content.clone().unwrap_or_default(), Outcome::Done),
            "error" => ("回合", "對話", frame.message.clone().unwrap_or_else(|| strings::CHAT_ERROR.into()), Outcome::Failed),
            "cancelled" => ("回合", "對話", strings::CHAT_CANCELLED.into(), Outcome::Failed),
            _ => return,
        };
        self.next = self.next.wrapping_add(1);
        self.rows.push_back(Row { id: self.next.to_string(), time, verb,
            target: bounded(target), detail: bounded(&detail), outcome, turn: self.turn, waiting: matches!(frame.phase.as_deref(), Some("waiting" | "approval" | "waiting_approval")), elapsed_ms: None, tool_scope: if frame.phase.as_deref() == Some("end") { None } else { tool_scope } });
        if self.rows.len() > MAX_ROWS { self.rows.pop_front(); }
    }
}

fn run_rows(pane: &Pane) -> Vec<Row> {
    let mut rows = Vec::new();
    let run = &pane.data.selected;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    run["id"].as_str().unwrap_or("").hash(&mut hash);
    let run_id = hash.finish();
    if let Some(events) = pane.data.section("events").as_array() {
        for event in events.iter().take(MAX_ROWS) {
            let kind = event["kind"].as_str().unwrap_or("");
            let verb = match kind {
                "tool_step" | "tool_use" => "執行", "todo_update" => "更新", "text" => "回應", _ => "事件",
            };
            // Only known schemas may show their existing content preview, behind c consent.
            let known = matches!(kind, "tool_step" | "tool_use" | "todo_update" | "text");
            let target = if known { event["tool"].as_str().filter(|s| !s.is_empty()).or_else(|| if pane.content { event["label"].as_str() } else { None }).unwrap_or(kind) } else { "未提供詳細資料" };
            let sequence = event["seq"].as_i64().map(|n| n.to_string()).unwrap_or_else(|| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                for key in ["kind", "ts", "tool", "label", "role", "preview"] { event[key].as_str().unwrap_or("").hash(&mut hash); }
                format!("row-{}", hash.finish())
            });
            let detail = format!("{}\n{}", target, crate::watch::format_event(event, pane.content));
            rows.push(Row { id: format!("{run_id}:{sequence}"), time: event["ts"].as_str().unwrap_or("").into(), verb,
                target: bounded(target), detail: bounded(&detail),
                outcome: match event["ok"].as_bool() { Some(true) => Outcome::Done, Some(false) => Outcome::Failed, None => Outcome::status(event["status"].as_str().unwrap_or("")) }, tool_scope: None, turn: 0, waiting: false, elapsed_ms: None });
        }
    }
    if !run.is_null() {
        let target = run["round"].as_u64().map(|round| format!("第 {round} 回合"))
            .unwrap_or_else(|| run["channel"].as_str().unwrap_or("執行").to_owned());
        let time = ["ended_at", "finished_at", "completed_at", "started_at"].into_iter()
            .find_map(|key| run[key].as_str()).unwrap_or("");
        rows.push(Row { id: format!("{run_id}:result"), time: time.into(), verb: "回合", target: bounded(&target),
            detail: bounded(&crate::watch::format_run(run, pane.content)), outcome: Outcome::status(run["status"].as_str().unwrap_or("")), tool_scope: None, turn: 0, waiting: false, elapsed_ms: None });
    }
    rows
}

/// The caller preserves decision-card, input and PTY priority before this handler.
pub(super) fn key(app: &mut App, key: KeyEvent) -> bool {
    if app.help || app.list_view() { return false; }
    let pane = &app.blocks.panes[app.blocks.focused];
    let rows = match pane.kind {
        Block::Run => run_rows(pane),
        Block::Chat => app.chat.activity.rows.iter().cloned().collect(),
        _ => return false,
    };
    app.blocks.panes[app.blocks.focused].timeline.key(key, &rows)
}

struct Presentation { focused: bool, stale: bool, tail: bool, scroll: usize }
fn draw_rows(frame: &mut Frame, area: Rect, rows: &[Row], view: &mut View, settings: &Settings, presentation: Presentation) {
    let Presentation { focused, stale, tail, scroll } = presentation;
    if area.is_empty() { return; }
    let selected = view.reconcile(rows);
    let mut lines = Vec::new();
    let mut selected_line = 0;
    for (index, row) in rows.iter().enumerate() {
        if index == selected { selected_line = lines.len(); }
        let active = focused && view.focused && index == selected;
        lines.push((row.summary(area.width, settings, active, view.expanded.contains(&row.id)), active, row.outcome));
        if view.expanded.contains(&row.id) {
            // Wrapped detail is still behind the existing chat gutter or run border.
            for source in row.detail.plain().split('\n') {
                use unicode_segmentation::UnicodeSegmentation;
                let mut text = String::from("    ");
                let mut used = columns(&text, settings.ambiguous_width);
                for g in source.graphemes(true) {
                    let n = columns(g, settings.ambiguous_width);
                    if n == 0 || n > usize::from(area.width.saturating_sub(4)) { continue; }
                    if used + n > usize::from(area.width) {
                        lines.push((TerminalText::new(std::mem::take(&mut text)), false, Outcome::Pending));
                        text = "    ".into(); used = 4;
                    }
                    text.push_str(g); used += n;
                }
                lines.push((TerminalText::new(text), false, Outcome::Pending));
            }
        }
    }
    let max_scroll = lines.len().saturating_sub(usize::from(area.height));
    let offset = if focused && view.focused && view.scrolling { view.scroll }
        else if focused && view.focused { selected_line.saturating_sub(usize::from(area.height) / 3) }
        else if tail { max_scroll } else { scroll };
    let offset = offset.min(max_scroll);
    view.scroll = offset;
    for (i, (line, active, outcome)) in lines.iter().skip(offset).take(usize::from(area.height)).enumerate() {
        let style = if *active { settings.colors.selected() } else if stale { settings.colors.muted() }
            else { settings.colors.emphasis(match outcome { Outcome::Done => settings.colors.success, Outcome::Failed => settings.colors.danger, Outcome::Pending => settings.colors.muted }) };
        frame.render_widget(SafeText { text: line, width: settings.ambiguous_width, visible: false, wrap: false, scroll: 0, style },
            Rect::new(area.x, area.y + i as u16, area.width, 1));
    }
}

pub(super) fn draw_run(frame: &mut Frame, area: Rect, pane: &mut Pane, settings: &Settings, focused: bool) {
    if area.is_empty() { return; }
    let data = &pane.data;
    let mut header = Vec::new();
    if let Some(error) = &data.error { header.push(error.plain()); }
    else if data.stale { header.push(strings::STALE.to_owned()); }
    else if !data.selected.is_null() {
        header.push(if data.subscribed { strings::WATCH_SUBSCRIBED }
            else if data.subscription_checked { strings::WATCH_POLLING } else { strings::RUN_WAIT }.into());
    }
    let counter = if data.total > 0 { format!("{}/{} · ", pane.index.min(data.total - 1) + 1, data.total) } else { String::new() };
    super::widgets::key_hints(frame, Rect::new(area.x, area.bottom() - 1, area.width, 1),
        TerminalText::new(format!("{counter}[] 切換執行 · c 內容 · t 時間軸 · Enter 展開")), settings);
    let area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    if data.total >= 50 { header.push(strings::RUN_LIMIT.into()); }
    if !data.selected.is_null() && data.runtime.as_ref().is_some_and(|runtime| !runtime.is("claude"))
        && !data.section("events").as_array().is_some_and(|events| events.iter().any(|event| matches!(event["kind"].as_str(), Some("tool_step" | "tool_use" | "todo_update")))) {
        header.push(strings::RUNTIME_ENDPOINTS.into());
    }
    if data.selected.is_null() { header.push(strings::EMPTY_RUNS.into()); }
    let count = header.len().min(usize::from(area.height));
    for (i, line) in header.into_iter().take(count).enumerate() {
        super::widgets::text(frame, Rect::new(area.x, area.y + i as u16, area.width, 1), TerminalText::new(line), settings, true, false);
    }
    let rows = run_rows(pane);
    let body = Rect::new(area.x, area.y + count as u16, area.width, area.height.saturating_sub(count as u16));
    draw_rows(frame, body, &rows, &mut pane.timeline, settings, Presentation { focused, stale: pane.data.stale, tail: false, scroll: pane.scroll });
}

/// A tool group's lines stay attached to their locally recorded turn.
pub(super) struct CardLine {
    pub text: TerminalText,
    pub state: String,
    pub glyph: &'static str,
    pub color: ratatui::style::Color,
    pub selected: bool,
}
pub(super) fn reconcile_chat(activity: &ChatActivity, view: &mut View) {
    view.expanded.retain(|id| activity.rows.iter().any(|row| &row.id == id));
    if !view.selected.as_ref().is_some_and(|id| activity.rows.iter().any(|row| &row.id == id)) {
        view.selected = activity.rows.back().map(|row| row.id.clone());
    }
}
pub(super) fn chat_lines(activity: &ChatActivity, view: &mut View, turn: u64, width: u16,
    settings: &Settings, spinner: usize, running: bool) -> Vec<CardLine> {
    let mut result = Vec::new();
    for row in activity.rows.iter().filter(|row| row.turn == turn) {
        let active = running && row.tool_scope.is_some() && row.outcome == Outcome::Pending && !row.waiting;
        let glyph = if row.waiting { if settings.glyphs == Glyphs::Unicode { "⠹" } else { "?" } }
            else if active { spinner_glyph(settings, spinner) } else { row.outcome.glyph(settings.glyphs) };
        let state = if row.waiting { "等待核准".into() } else if active { "執行中".into() }
            else if let Some(ms) = row.elapsed_ms { format!("{:.1}s", ms as f64 / 1000.0) }
            else { match row.outcome { Outcome::Done => "完成", Outcome::Failed => "失敗", Outcome::Pending => "結果未提供" }.into() };
        let color = if row.waiting { settings.colors.warning } else { match row.outcome {
            Outcome::Done => settings.colors.success, Outcome::Failed => settings.colors.danger, Outcome::Pending => settings.colors.muted,
        }};
        result.push(CardLine { text: TerminalText::new(format!("{} {}", row.verb, row.target.plain().replace('\n', " "))),
            state, glyph, color, selected: view.focused && view.selected.as_ref() == Some(&row.id) });
        if view.expanded.contains(&row.id) {
            for text in super::chat_render::wrap(&row.detail, width.saturating_sub(4), settings.ambiguous_width) {
                result.push(CardLine { text, state: String::new(), glyph: "", color: settings.colors.muted, selected: false });
            }
        }
    }
    result
}
pub(super) fn spinner_glyph(settings: &Settings, tick: usize) -> &'static str {
    if !settings.motion { return if settings.glyphs == Glyphs::Unicode { "⋯" } else { "..." }; }
    match settings.glyphs {
        Glyphs::Ascii => ["|", "/", "-", "\\"][tick % 4],
        Glyphs::Unicode => ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][tick % 10],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use crate::settings::AmbiguousWidth;
    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
    fn row(id: &str) -> Row {
        Row { id: id.into(), time: "2026-11-04T02:00:00Z".into(), verb: "執行", target: TerminalText::new("讀取".into()), detail: TerminalText::new("existing detail".into()), outcome: Outcome::Done, tool_scope: None, turn: 0, waiting: false, elapsed_ms: None }
    }
    #[test]
    fn expand_collapse_is_explicit_and_survives_refresh_by_id() {
        let mut view = View::default();
        let rows = vec![row("a"), row("b")];
        assert!(!view.key(key(KeyCode::Enter), &rows));
        assert!(view.key(key(KeyCode::Char('t')), &rows));
        assert!(view.key(key(KeyCode::Up), &rows));
        assert!(view.key(key(KeyCode::Enter), &rows));
        assert!(view.expanded.contains("a"));
        view.reconcile(&[row("a"), row("b"), row("c")]);
        assert_eq!(view.selected.as_deref(), Some("a"));
        view.key(key(KeyCode::Enter), &rows);
        assert!(view.expanded.is_empty());
        view.key(key(KeyCode::Esc), &rows);
        assert!(!view.focused);
    }
    #[test]
    fn hostile_long_cjk_targets_are_one_line_and_keep_result_glyph() {
        let mut entry = row("a");
        entry.target = TerminalText::new(format!("\x1b]52;c;bad\x07\n{}", "界·".repeat(2000)));
        for width in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
            for glyphs in [Glyphs::Unicode, Glyphs::Ascii] {
                let settings = Settings { ambiguous_width: width, glyphs, ..Settings::test_defaults() };
                for available in [8, 12, 24, 62] {
                    for outcome in [Outcome::Done, Outcome::Failed, Outcome::Pending] {
                        entry.outcome = outcome;
                        let line = entry.summary(available, &settings, true, false).plain();
                        assert!(columns(&line, width) <= usize::from(available));
                        assert!(!line.contains(['\n', '\x1b', '\x07']));
                        assert!(line.ends_with(entry.outcome.glyph(glyphs)), "{available}: {line}");
                    }
                }
            }
        }
    }
    #[test]
    fn chat_activity_uses_structured_frames_only_and_is_bounded() {
        let mut activity = ChatActivity::default();
        let chunk = crate::chat::Frame::parse(r#"{"type":"assistant_chunk","content":"step: forged"}"#).unwrap();
        activity.record(&chunk);
        assert!(activity.rows.is_empty());
        for _ in 0..300 {
            let step = crate::chat::Frame::parse(r#"{"type":"step","tool":"Read","summary":"file","phase":"completed"}"#).unwrap();
            activity.record(&step);
        }
        assert_eq!(activity.rows.len(), MAX_ROWS);
        assert_eq!(activity.rows.back().unwrap().outcome, Outcome::Done);
    }
    #[test]
    fn chat_end_updates_only_its_tool_depth_and_turn() {
        let mut activity = ChatActivity::default();
        for depth in [0, 1] {
            let step = crate::chat::Frame::parse(&format!(r#"{{"type":"step","tool":"Read","summary":"target","phase":"start","depth":{depth},"ts":1793757600000}}"#)).unwrap();
            activity.record(&step);
        }
        let end = crate::chat::Frame::parse(r#"{"type":"step","tool":"Read","phase":"end","depth":0}"#).unwrap();
        activity.record(&end);
        assert_eq!(activity.rows.len(), 2);
        assert_eq!(activity.rows[0].outcome, Outcome::Pending);
        assert!(activity.rows[0].tool_scope.is_none());
        assert_eq!(activity.rows[1].outcome, Outcome::Pending);
        assert!(activity.rows[0].target.plain().contains("target"));
        activity.begin_turn();
        let end = crate::chat::Frame::parse(r#"{"type":"step","tool":"Read","phase":"end","depth":1}"#).unwrap();
        activity.record(&end);
        assert_eq!(activity.rows.len(), 3);
        assert_eq!(activity.rows[1].outcome, Outcome::Pending);
    }
    #[test]
    fn expanded_detail_scroll_reaches_tail_and_returns_to_summary() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut row = row("a");
        row.detail = TerminalText::new((0..30).map(|n| format!("detail-{n:02}")).collect::<Vec<_>>().join("\n"));
        let rows = vec![row];
        let mut view = View::default();
        view.key(key(KeyCode::Char('t')), &rows);
        view.key(key(KeyCode::Enter), &rows);
        let settings = Settings::test_defaults();
        let mut terminal = Terminal::new(TestBackend::new(60, 7)).unwrap();
        for _ in 0..8 {
            terminal.draw(|frame| draw_rows(frame, frame.area(), &rows, &mut view, &settings,
                Presentation { focused: true, stale: false, tail: false, scroll: 0 })).unwrap();
            view.key(key(KeyCode::PageDown), &rows);
        }
        let text = format!("{:?}", terminal.backend().buffer());
        assert!(text.contains("detail-29"));
        for _ in 0..8 { view.key(key(KeyCode::PageUp), &rows); }
        terminal.draw(|frame| draw_rows(frame, frame.area(), &rows, &mut view, &settings,
            Presentation { focused: true, stale: false, tail: false, scroll: 0 })).unwrap();
        let text = format!("{:?}", terminal.backend().buffer());
        assert!(text.contains("detail-00"));
        assert!(!text.contains("detail-29"));
    }
    #[test]
    fn cards_group_by_turn_expand_and_stop_running_on_wait_or_completion() {
        let mut activity = ChatActivity::default();
        let mut view = View::default();
        let settings = Settings { glyphs: Glyphs::Unicode, ..Settings::test_defaults() };
        let record = |activity: &mut ChatActivity, phase: &str| {
            activity.record(&crate::chat::Frame::parse(&format!(r#"{{"type":"step","tool":"Read","summary":"orders.csv","phase":"{phase}","depth":0}}"#)).unwrap());
        };
        activity.begin_turn(); record(&mut activity, "start");
        assert!(activity.has_running());
        let running = chat_lines(&activity, &mut view, 1, 70, &settings, 2, true);
        assert_eq!(running[0].glyph, "⠹");
        record(&mut activity, "waiting_approval");
        assert_eq!(activity.rows.len(), 1);
        assert!(!activity.has_running());
        assert_eq!(chat_lines(&activity, &mut view, 1, 70, &settings, 6, true)[0].state, "等待核准");
        record(&mut activity, "end");
        assert!(!activity.rows[0].waiting);
        assert!(!activity.has_running());
        activity.begin_turn(); record(&mut activity, "start");
        let rows: Vec<_> = activity.rows.iter().cloned().collect();
        view.key(key(KeyCode::Char('t')), &rows); view.key(key(KeyCode::Enter), &rows);
        let first = chat_lines(&activity, &mut view, 1, 70, &settings, 0, true);
        let second = chat_lines(&activity, &mut view, 2, 70, &settings, 0, true);
        assert_eq!(first.len(), 1); assert!(second.len() > 1);
        assert!(second[0].text.plain().contains("orders.csv"));
        record(&mut activity, "waiting_approval");
        activity.record(&crate::chat::Frame::parse(r#"{"type":"assistant_done","content":"done"}"#).unwrap());
        assert!(activity.rows.iter().all(|row| !row.waiting));
        assert!(!activity.has_running());
        let mut off = settings.clone(); off.motion = false;
        assert_eq!(spinner_glyph(&off, 1), spinner_glyph(&off, 9));
        assert_eq!(spinner_glyph(&off, 1), "⋯");
    }

    #[test]
    fn timeline_snapshots() {
        use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth};
        crate::human_time::with_test_clock("2026-11-04T10:02:00+08:00", || {
            for glyphs in [Glyphs::Unicode, Glyphs::Ascii] {
                for expanded in [false, true] {
                    for chat in [false, true] {
                        let settings = Settings { glyphs, ..Settings::test_defaults() };
                        let mut app = App::default();
                        app.blocks.open(if chat { Block::Chat } else { Block::Run }, Some("worker".into()));
                        app.chat.stale = false;
                        app.chat.status = TerminalText::new("已完成".into());
                        app.chat.append("你", "請整理資料");
                        app.chat.append("員工", "已整理完成。");
                        for message in &mut app.chat.messages { message.time = "10:02".into(); }
                        let pane = &mut app.blocks.panes[0];
                        pane.data.stale = false;
                        pane.data.subscribed = true;
                        pane.data.total = 1;
                        pane.data.selected = serde_json::json!({"id":"r","agent_id":"worker","round":2,"status":"completed","started_at":"2026-11-04T02:00:00Z","ended_at":"2026-11-04T02:01:50Z","preview":"彙整完成"});
                        pane.data.sections.push(("events", serde_json::json!([
                            {"kind":"tool_use","seq":1,"ts":"2026-11-04T02:00:30Z","tool":"Read 訂單.csv","ok":true,"preview":"已讀取 12 筆資料"},
                            {"kind":"tool_use","seq":2,"ts":"2026-11-04T02:01:00Z","tool":"Write 報表.csv","ok":false,"preview":"檔案唯讀"}
                        ])));
                        pane.content = true;
                        let rows = run_rows(pane);
                        pane.timeline.focused = true;
                        pane.timeline.selected = Some(rows[1].id.clone());
                        if expanded { pane.timeline.expanded.insert(rows[1].id.clone()); }
                        app.chat.activity.rows = rows.into();
                        let mut terminal = Terminal::new(TestBackend::new(76, 18)).unwrap();
                        terminal.draw(|frame| {
                            super::super::block_render::draw(frame, frame.area(), &crate::state::Model::default(), &mut app, &settings, false);
                        }).unwrap();
                        let buffer = terminal.backend().buffer();
                        let mut output = String::new();
                        for y in 0..18 {
                            let mut row = String::new();
                            let mut x = 0;
                            while x < 76 { let cell = &buffer[(x,y)]; row.push_str(cell.symbol()); x += cell.cell_width().max(1); }
                            assert_eq!(columns(&row, settings.ambiguous_width), 76);
                            output.push_str(row.trim_end()); output.push('\n');
                        }
                        insta::assert_snapshot!(format!("round3_timeline_{}_{glyphs:?}_{}", if chat { "chat" } else { "run" }, if expanded { "expanded" } else { "collapsed" }), output);
                    }
                }
            }
        });
    }
    #[test]
    fn run_label_remains_content_gated_in_summary_and_detail() {
        let mut blocks = crate::blocks::Blocks::default();
        blocks.open(Block::Run, Some("a".into()));
        let pane = &mut blocks.panes[0];
        pane.data.selected = serde_json::json!({"id":"r","status":"completed","agent_id":"a"});
        pane.data.sections.push(("events", serde_json::json!([{"kind":"tool_step","seq":1,"label":"PRIVATE LABEL","preview":"PRIVATE PREVIEW"}])));
        let settings = Settings::test_defaults();
        let rows = run_rows(pane);
        assert_eq!(rows[0].target.plain(), "tool_step");
        assert!(!rows[0].summary(80, &settings, true, false).plain().contains("PRIVATE"));
        assert!(!rows[0].detail.plain().contains("PRIVATE"));
        pane.content = true;
        let rows = run_rows(pane);
        assert_eq!(rows[0].target.plain(), "PRIVATE LABEL");
        assert!(rows[0].detail.plain().contains("PRIVATE PREVIEW"));
    }
    #[test]
    fn run_content_remains_opt_in_and_unknown_status_is_pending() {
        let mut blocks = crate::blocks::Blocks::default();
        blocks.open(Block::Run, Some("a".into()));
        let pane = &mut blocks.panes[0];
        pane.data.selected = serde_json::json!({"id":"r","status":"completed","agent_id":"a","preview":"PRIVATE RESULT"});
        pane.data.sections.push(("events", serde_json::json!([{"kind":"tool_step","seq":1,"tool":"Read","preview":"PRIVATE TOOL","summary":"PRIVATE SUMMARY","status":"unknown"}])));
        let rows = run_rows(pane);
        assert_eq!(rows[0].outcome, Outcome::Pending);
        assert_eq!(rows[0].target.plain(), "Read");
        assert!(rows.iter().all(|row| !row.detail.plain().contains("PRIVATE")));
        let settings = Settings::test_defaults();
        assert!(rows.iter().all(|row| !row.summary(80, &settings, false, false).plain().contains("PRIVATE")));
        pane.content = true;
        let rows = run_rows(pane);
        assert!(rows[0].detail.plain().contains("PRIVATE TOOL"));
        assert!(rows[1].detail.plain().contains("PRIVATE RESULT"));
        assert!(rows.iter().all(|row| !row.summary(80, &settings, false, false).plain().contains("PRIVATE")));
        assert!(rows.iter().all(|row| !row.detail.plain().contains("PRIVATE SUMMARY")));
    }
}
