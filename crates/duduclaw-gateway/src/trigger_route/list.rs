//! The `@` list: the employees a sender may call, rendered for each channel.

use duduclaw_agent::registry::AgentRegistry;
use serde_json::{Value, json};

use super::texts::{self, Lang};

/// One callable employee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableAgent {
    pub id: String,
    pub display_name: String,
    pub icon: String,
    /// The text that calls exactly this employee: `@display name` when that
    /// name is unique among employees, else `@id`.
    pub call_text: String,
}

/// LINE shows at most 13 quick-reply buttons.
pub const LINE_QUICK_REPLY_MAX: usize = 13;
/// LINE quick-reply label limit (characters).
const LINE_LABEL_MAX: usize = 20;
/// Telegram `callback_data` limit (bytes).
const TELEGRAM_CALLBACK_MAX: usize = 64;
/// Postback prefix for "next page of the `@` list" on LINE.
pub const LINE_PAGE_POSTBACK: &str = "duduclaw:mention_page:";
/// Callback prefix for picking an employee from the Telegram `@` list.
pub const TELEGRAM_PICK_CALLBACK: &str = "duduclaw:mention:";

/// Build the list entries for `ids` (already permission-filtered).
pub fn callable_agents(reg: &AgentRegistry, ids: &[String]) -> Vec<CallableAgent> {
    let operational: Vec<_> = reg
        .list()
        .into_iter()
        .filter(|a| a.config.agent.status.is_operational())
        .collect();
    ids.iter()
        .filter_map(|id| reg.get(id))
        .map(|a| {
            let id = a.config.agent.name.clone();
            let display = super::display_name(reg, &id);
            let unique = operational
                .iter()
                .filter(|o| {
                    o.config
                        .agent
                        .display_name
                        .trim()
                        .eq_ignore_ascii_case(&display)
                })
                .count()
                <= 1;
            let call_text = if unique {
                format!("@{display}")
            } else {
                format!("@{id}")
            };
            CallableAgent {
                id,
                display_name: display,
                icon: a.config.agent.icon.clone(),
                call_text,
            }
        })
        .collect()
}

/// Plain-text list, for channels without buttons.
pub fn text_list(agents: &[CallableAgent], lang: Lang) -> String {
    if agents.is_empty() {
        return texts::nobody_callable(lang).to_string();
    }
    let mut out = texts::list_heading(lang, &agents[0].call_text);
    for a in agents {
        out.push_str("\n• ");
        out.push_str(&a.call_text);
    }
    out
}

/// Intro line shown above a button list.
pub fn list_intro(agents: &[CallableAgent], lang: Lang) -> String {
    if agents.is_empty() {
        texts::nobody_callable(lang).to_string()
    } else {
        texts::list_intro(lang).to_string()
    }
}

/// The "more" button label.
fn more_label(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "更多",
        Lang::En => "More",
        Lang::Ja => "もっと見る",
    }
}

/// LINE text message with the `page`-th (0-based) page of quick-reply
/// buttons. Each button sends `@name` as the user. When the list does not
/// fit, the last button is "更多" (a postback for the next page).
pub fn line_list_message(agents: &[CallableAgent], page: usize, lang: Lang) -> Value {
    let per_page = LINE_QUICK_REPLY_MAX - 1;
    let start = page.saturating_mul(per_page).min(agents.len());
    let remaining = &agents[start..];
    let (shown, more) = if remaining.len() > LINE_QUICK_REPLY_MAX {
        (&remaining[..per_page], true)
    } else {
        (remaining, false)
    };
    let mut items: Vec<Value> = shown
        .iter()
        .map(|a| {
            json!({
                "type": "action",
                "action": {
                    "type": "message",
                    "label": duduclaw_core::truncate_chars(&a.display_name, LINE_LABEL_MAX),
                    "text": a.call_text,
                }
            })
        })
        .collect();
    if more {
        items.push(json!({
            "type": "action",
            "action": {
                "type": "postback",
                "label": more_label(lang),
                "data": format!("{LINE_PAGE_POSTBACK}{}", page + 1),
                "displayText": more_label(lang),
            }
        }));
    }
    let mut msg = json!({ "type": "text", "text": list_intro(agents, lang) });
    if !items.is_empty() {
        msg["quickReply"] = json!({ "items": items });
    }
    msg
}

/// Parse a LINE "more" postback into the page it asks for.
pub fn parse_line_page(data: &str) -> Option<usize> {
    data.strip_prefix(LINE_PAGE_POSTBACK)?.parse().ok()
}

/// Telegram inline keyboard (two buttons per row). Pressing one arms the
/// one-shot addressee; employees whose id does not fit `callback_data` are
/// left out of the buttons (they stay callable by typing). `None` when no
/// button fits: the caller sends the text list instead.
pub fn telegram_keyboard(agents: &[CallableAgent]) -> Option<Value> {
    let buttons: Vec<Value> = agents
        .iter()
        .filter_map(|a| {
            let data = format!("{TELEGRAM_PICK_CALLBACK}{}", a.id);
            (data.len() <= TELEGRAM_CALLBACK_MAX).then(|| {
                json!({
                    "text": duduclaw_core::truncate_chars(&a.display_name, 32),
                    "callback_data": data,
                })
            })
        })
        .collect();
    let rows: Vec<Value> = buttons
        .chunks(2)
        .map(|c| Value::Array(c.to_vec()))
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(json!({ "inline_keyboard": rows }))
}

/// Parse a Telegram pick callback into the employee id.
pub fn parse_telegram_pick(data: &str) -> Option<&str> {
    data.strip_prefix(TELEGRAM_PICK_CALLBACK)
        .filter(|id| !id.is_empty())
}
