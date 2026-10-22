//! How a messaging app renders the thinking status (pure functions).
//!
//! Every summary that reaches here already went through the turn's gate
//! (masking, CONTRACT / guardrail checks); this module only caps it to the
//! platform's limits (by chars, CJK-safe) and escapes it for the platform's
//! markup so it cannot break out of its quote / spoiler / block.

use crate::trigger_route::texts::Lang;

/// Summary cap where the platform can fold it away (Telegram expandable
/// blockquote; message limit 4,096).
pub const TELEGRAM_SUMMARY_CHARS: usize = 3_000;
/// Discord spoiler (message limit 2,000).
pub const DISCORD_SUMMARY_CHARS: usize = 1_500;
/// Slack context block element (text limit 3,000).
pub const SLACK_SUMMARY_CHARS: usize = 2_500;
/// Platforms with no way to fold text away.
pub const PLAIN_SUMMARY_CHARS: usize = 300;
/// The live progress line.
pub const LINE_CHARS: usize = 80;

/// What the status shows right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// `false` while thinking, `true` once the answer is ready.
    pub done: bool,
    pub seconds: u64,
    /// Latest masked progress line (summary audience only).
    pub line: Option<String>,
    /// Masked summary (summary audience, `done` only).
    pub summary: Option<String>,
    pub lang: Lang,
}

fn duration(seconds: u64, lang: Lang) -> String {
    let (m, s) = (seconds / 60, seconds % 60);
    match (lang, m) {
        (Lang::En, 0) => format!("{s}s"),
        (Lang::En, _) => format!("{m}m {s}s"),
        (_, 0) => format!("{s} 秒"),
        (_, _) => format!("{m} 分 {s} 秒"),
    }
}

/// `💭 思考中… 12 秒` / `💭 思考了 12 秒` (and en / ja).
pub fn header(view: &StatusView) -> String {
    let d = duration(view.seconds, view.lang);
    match (view.lang, view.done) {
        (Lang::ZhTw, false) => format!("💭 思考中… {d}"),
        (Lang::ZhTw, true) => format!("💭 思考了 {d}"),
        (Lang::En, false) => format!("💭 Thinking… {d}"),
        (Lang::En, true) => format!("💭 Thought for {d}"),
        (Lang::Ja, false) => format!("💭 考え中… {d}"),
        (Lang::Ja, true) => format!("💭 {d}考えました"),
    }
}

/// `text` capped to `max` chars, with an ellipsis when cut.
pub fn cap(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut = duduclaw_core::truncate_chars(text, max.saturating_sub(1));
    format!("{}…", cut.trim_end())
}

fn live_line(view: &StatusView) -> Option<String> {
    view.line
        .as_deref()
        .filter(|_| !view.done)
        .map(|l| cap(l, LINE_CHARS))
        .filter(|l| !l.is_empty())
}

fn done_summary(view: &StatusView, max: usize) -> Option<String> {
    view.summary
        .as_deref()
        .filter(|_| view.done)
        .map(|s| cap(s, max))
        .filter(|s| !s.is_empty())
}

/// Plain text (no markup): header, then the live line or a capped summary.
pub fn plain(view: &StatusView, summary_max: usize) -> String {
    let mut out = header(view);
    if let Some(l) = live_line(view) {
        out.push('\n');
        out.push_str(&l);
    }
    if let Some(s) = done_summary(view, summary_max) {
        out.push('\n');
        out.push_str(&s);
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Telegram: `(text, parse_mode)`. The summary sits in an expandable
/// blockquote (HTML parse mode); everything is HTML-escaped.
pub fn telegram(view: &StatusView) -> (String, &'static str) {
    let mut out = html_escape(&header(view));
    if let Some(l) = live_line(view) {
        out.push('\n');
        out.push_str(&html_escape(&l));
    }
    if let Some(s) = done_summary(view, TELEGRAM_SUMMARY_CHARS) {
        out.push_str("\n<blockquote expandable>");
        out.push_str(&html_escape(&s));
        out.push_str("</blockquote>");
    }
    (out, "HTML")
}

/// Discord message content: the summary as a spoiler. `||` inside the text
/// is broken up so it cannot close the spoiler early. (Mentions are
/// disabled by the caller with `allowed_mentions`.)
pub fn discord(view: &StatusView) -> String {
    let mut out = header(view);
    if let Some(l) = live_line(view) {
        out.push_str("\n-# ");
        out.push_str(&l.replace('\n', " "));
    }
    if let Some(s) = done_summary(view, DISCORD_SUMMARY_CHARS) {
        out.push_str("\n||");
        out.push_str(&s.replace("||", "|\u{200B}|"));
        out.push_str("||");
    }
    out
}

fn slack_escape(s: &str) -> String {
    html_escape(s)
}

/// Slack `(fallback text, blocks)`: a section with the header and a small
/// context block with the line or the summary.
pub fn slack(view: &StatusView) -> (String, serde_json::Value) {
    let head = header(view);
    let detail = live_line(view).or_else(|| done_summary(view, SLACK_SUMMARY_CHARS));
    let mut blocks = vec![serde_json::json!({
        "type": "section",
        "text": {"type": "mrkdwn", "text": slack_escape(&head)}
    })];
    if let Some(d) = detail {
        blocks.push(serde_json::json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": slack_escape(&d)}]
        }));
    }
    (head, serde_json::Value::Array(blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(done: bool, seconds: u64) -> StatusView {
        StatusView {
            done,
            seconds,
            line: Some("正在比對 <兩份> 報表".into()),
            summary: Some("先比對 & 再寄出".into()),
            lang: Lang::ZhTw,
        }
    }

    #[test]
    fn headers_in_three_languages() {
        let mut v = view(false, 12);
        assert_eq!(header(&v), "💭 思考中… 12 秒");
        v.done = true;
        v.seconds = 75;
        assert_eq!(header(&v), "💭 思考了 1 分 15 秒");
        v.lang = Lang::En;
        assert_eq!(header(&v), "💭 Thought for 1m 15s");
        v.done = false;
        v.seconds = 3;
        assert_eq!(header(&v), "💭 Thinking… 3s");
        v.lang = Lang::Ja;
        assert_eq!(header(&v), "💭 考え中… 3 秒");
        v.done = true;
        assert_eq!(header(&v), "💭 3 秒考えました");
    }

    #[test]
    fn live_shows_the_line_done_shows_the_summary() {
        assert_eq!(plain(&view(false, 4), 300), "💭 思考中… 4 秒\n正在比對 <兩份> 報表");
        assert_eq!(plain(&view(true, 9), 300), "💭 思考了 9 秒\n先比對 & 再寄出");
        let bare = StatusView {
            line: None,
            summary: None,
            ..view(true, 9)
        };
        assert_eq!(plain(&bare, 300), "💭 思考了 9 秒");
    }

    #[test]
    fn caps_are_by_chars_and_cjk_safe() {
        let long: String = "嘟🐾".repeat(400);
        let v = StatusView {
            summary: Some(long.clone()),
            ..view(true, 1)
        };
        let out = plain(&v, PLAIN_SUMMARY_CHARS);
        let summary = out.split_once('\n').unwrap().1;
        assert_eq!(summary.chars().count(), PLAIN_SUMMARY_CHARS);
        assert!(summary.ends_with('…'));
        let line = StatusView {
            line: Some("行".repeat(200)),
            ..view(false, 1)
        };
        let out = plain(&line, PLAIN_SUMMARY_CHARS);
        assert_eq!(out.split_once('\n').unwrap().1.chars().count(), LINE_CHARS);
        let (tg, _) = telegram(&StatusView {
            summary: Some(long.clone()),
            ..view(true, 1)
        });
        assert!(tg.chars().count() < 4_096);
        assert!(discord(&StatusView { summary: Some(long), ..view(true, 1) }).chars().count() < 2_000);
    }

    #[test]
    fn telegram_escapes_and_folds_the_summary() {
        let (live, mode) = telegram(&view(false, 2));
        assert_eq!(mode, "HTML");
        assert_eq!(live, "💭 思考中… 2 秒\n正在比對 &lt;兩份&gt; 報表");
        let (done, _) = telegram(&StatusView {
            summary: Some("a</blockquote><b>x".into()),
            ..view(true, 2)
        });
        assert_eq!(
            done,
            "💭 思考了 2 秒\n<blockquote expandable>a&lt;/blockquote&gt;&lt;b&gt;x</blockquote>"
        );
    }

    #[test]
    fn discord_spoiler_cannot_be_closed_early() {
        let done = discord(&StatusView {
            summary: Some("a||b".into()),
            ..view(true, 2)
        });
        assert_eq!(done, "💭 思考了 2 秒\n||a|\u{200B}|b||");
        assert_eq!(discord(&view(false, 2)), "💭 思考中… 2 秒\n-# 正在比對 <兩份> 報表");
    }

    #[test]
    fn slack_puts_details_in_a_context_block() {
        let (text, blocks) = slack(&view(true, 2));
        assert_eq!(text, "💭 思考了 2 秒");
        assert_eq!(blocks[1]["type"], "context");
        assert_eq!(blocks[1]["elements"][0]["text"], "先比對 &amp; 再寄出");
        let (_, live) = slack(&view(false, 2));
        assert_eq!(live[1]["elements"][0]["text"], "正在比對 &lt;兩份&gt; 報表");
        let (_, bare) = slack(&StatusView {
            line: None,
            summary: None,
            ..view(true, 2)
        });
        assert_eq!(bare.as_array().unwrap().len(), 1);
    }
}
