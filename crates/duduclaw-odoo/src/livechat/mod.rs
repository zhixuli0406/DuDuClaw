//! Odoo Live Chat bridge — config, audience routing and message text.
//!
//! DuDuClaw joins an Odoo website Live Chat as an ordinary **operator**: a
//! dedicated internal Odoo user whose presence the gateway keeps `online`
//! (`bus.presence.update_presence`). Odoo then assigns visitor sessions to it
//! like to any human operator — directly, or when a Chatbot script reaches a
//! "Forward to operator" step. The gateway reads visitor messages over
//! JSON-RPC and answers with `discuss.channel.message_post`.
//!
//! Why an operator and not a Chatbot script: Odoo's widget locks the visitor's
//! input box once a script's last step has run ("This conversation has
//! ended.") unless an operator was found, so a script alone cannot carry a
//! free conversation. Verified against Odoo 18.0 (`im_livechat` sources and a
//! live instance, 2026-10-12).
//!
//! Audience routing ("分眾"): each session is matched once, on its first
//! visitor message, against ordered [`AudienceRule`]s; the first enabled rule
//! whose criteria all hold picks the AI employee, otherwise `default_agent`.
//! The choice is sticky for the session.

pub mod rpc;

use serde::{Deserialize, Serialize};

/// `config.toml` section name. Kept separate from `[odoo]` because
/// `odoo.configure` rewrites that whole section.
pub const SECTION: &str = "odoo_livechat";

/// Poll interval bounds (seconds). A live chat needs a short interval; the
/// lower bound keeps a misconfiguration from hammering the ERP.
pub const POLL_INTERVAL_MIN: u64 = 2;
pub const POLL_INTERVAL_MAX: u64 = 60;
pub const POLL_INTERVAL_DEFAULT: u64 = 4;

/// Upper bounds for dashboard-supplied lists and strings.
pub const MAX_RULES: usize = 50;
pub const MAX_LIST_ITEMS: usize = 50;
pub const MAX_TEXT_CHARS: usize = 500;

/// `[odoo_livechat]` in `config.toml`. Default is fully off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LivechatConfig {
    pub enabled: bool,
    /// Login of the dedicated Odoo bot user. Empty ⇒ reuse the `[odoo]`
    /// connection user (not recommended: visitors then see that user's name,
    /// and that user's own messages could not be told apart from the bot's).
    pub bot_username: String,
    /// Only serve these `im_livechat.channel` ids. Empty ⇒ every live chat
    /// session Odoo assigns to the bot user.
    pub livechat_channel_ids: Vec<i64>,
    pub poll_interval_seconds: u64,
    /// AI employee used when no audience rule matches. Empty ⇒ main agent.
    pub default_agent: String,
    /// Posted once when the bot first sees a new session. Empty ⇒ none.
    pub greeting: String,
    /// Odoo users (`res.users` ids) invited when a visitor asks for a human.
    pub handoff_user_ids: Vec<i64>,
    /// Visitor phrases that request a human.
    pub handoff_keywords: Vec<String>,
    /// Posted to the visitor when a handoff is triggered.
    pub handoff_message: String,
    /// Ordered audience rules; first match wins.
    pub audience_rules: Vec<AudienceRule>,
}

impl Default for LivechatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_username: String::new(),
            livechat_channel_ids: Vec::new(),
            poll_interval_seconds: POLL_INTERVAL_DEFAULT,
            default_agent: String::new(),
            greeting: String::new(),
            handoff_user_ids: Vec::new(),
            handoff_keywords: vec![
                "真人".to_string(),
                "人工客服".to_string(),
                "轉接客服".to_string(),
                "human".to_string(),
            ],
            handoff_message: "好的，已為您轉接真人客服，請稍候。".to_string(),
            audience_rules: Vec::new(),
        }
    }
}

impl LivechatConfig {
    /// Parse `[odoo_livechat]` from a config table. Missing or malformed ⇒
    /// default (disabled), so a bad section can never start the bridge.
    pub fn from_toml(table: &toml::Table) -> Self {
        table
            .get(SECTION)
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default()
    }

    pub fn effective_poll_interval(&self) -> u64 {
        self.poll_interval_seconds
            .clamp(POLL_INTERVAL_MIN, POLL_INTERVAL_MAX)
    }

    /// Whether `channel_id` (an `im_livechat.channel`) is served.
    pub fn serves_livechat_channel(&self, channel_id: Option<i64>) -> bool {
        if self.livechat_channel_ids.is_empty() {
            return true;
        }
        channel_id.is_some_and(|id| self.livechat_channel_ids.contains(&id))
    }

    /// Validate what the dashboard is about to save. Returns the first
    /// problem in plain zh-TW, suitable for the settings page.
    pub fn validate(&self) -> Result<(), String> {
        if self.bot_username.chars().count() > 256 {
            return Err("機器人帳號過長（最多 256 字）".into());
        }
        if self.default_agent.chars().count() > 128 {
            return Err("預設 AI 員工名稱過長".into());
        }
        if self
            .handoff_keywords
            .iter()
            .any(|k| k.trim().is_empty() || k.chars().count() > 80)
        {
            return Err("轉接關鍵字不可空白且最多 80 字".into());
        }
        for (label, text) in [
            ("開場白", &self.greeting),
            ("轉接訊息", &self.handoff_message),
        ] {
            if text.chars().count() > MAX_TEXT_CHARS {
                return Err(format!("{label}過長（最多 {MAX_TEXT_CHARS} 字）"));
            }
        }
        if self.livechat_channel_ids.len() > MAX_LIST_ITEMS
            || self.handoff_user_ids.len() > MAX_LIST_ITEMS
            || self.handoff_keywords.len() > MAX_LIST_ITEMS
        {
            return Err(format!("清單項目過多（每項最多 {MAX_LIST_ITEMS} 個）"));
        }
        if self.audience_rules.len() > MAX_RULES {
            return Err(format!("分眾規則過多（最多 {MAX_RULES} 條）"));
        }
        for (i, rule) in self.audience_rules.iter().enumerate() {
            rule.validate()
                .map_err(|e| format!("第 {} 條分眾規則：{e}", i + 1))?;
        }
        Ok(())
    }
}

/// Which visitors a rule applies to, by login state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisitorKind {
    #[default]
    Any,
    /// Visitor is logged in to the Odoo website (a `res.partner`).
    LoggedIn,
    /// Anonymous visitor (an Odoo `mail.guest`).
    Anonymous,
}

/// One audience segment → AI employee mapping. Every non-empty criterion
/// must hold (AND); inside one criterion any listed value may match (OR).
/// A rule with no criteria matches every visitor (a catch-all).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudienceRule {
    pub name: String,
    pub agent: String,
    pub enabled: bool,
    /// `im_livechat.channel` ids (Odoo already maps website URLs and
    /// countries to channels and chatbots with its own channel rules).
    pub livechat_channel_ids: Vec<i64>,
    /// The Chatbot script that forwarded the visitor (its branch = the
    /// visitor's self-declared intent).
    pub chatbot_script_ids: Vec<i64>,
    /// ISO country codes (`TW`, `JP`), from Odoo's GeoIP of the visitor.
    pub country_codes: Vec<String>,
    /// Odoo language codes. `zh_TW` matches exactly; `zh` matches any `zh_*`.
    pub langs: Vec<String>,
    pub visitor: VisitorKind,
    /// `res.partner.category` names; logged-in visitors only.
    pub partner_tags: Vec<String>,
    /// Phrases looked for in the visitor messages that are still unanswered
    /// when the session is routed (`TurnInput` pending text). Answers clicked
    /// in a Chatbot script are followed by a script line, end up in
    /// `earlier` and are not matched; route on `chatbot_script_ids` instead.
    pub keywords: Vec<String>,
}

impl Default for AudienceRule {
    fn default() -> Self {
        Self {
            name: String::new(),
            agent: String::new(),
            enabled: true,
            livechat_channel_ids: Vec::new(),
            chatbot_script_ids: Vec::new(),
            country_codes: Vec::new(),
            langs: Vec::new(),
            visitor: VisitorKind::Any,
            partner_tags: Vec::new(),
            keywords: Vec::new(),
        }
    }
}

impl AudienceRule {
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("請填寫規則名稱".into());
        }
        if self.name.chars().count() > 80 {
            return Err("規則名稱過長（最多 80 字）".into());
        }
        if self.agent.trim().is_empty() {
            return Err("請選擇負責的 AI 員工".into());
        }
        if self.agent.chars().count() > 128 {
            return Err("AI 員工名稱過長".into());
        }
        let lists = [
            self.livechat_channel_ids.len(),
            self.chatbot_script_ids.len(),
            self.country_codes.len(),
            self.langs.len(),
            self.partner_tags.len(),
            self.keywords.len(),
        ];
        if lists.iter().any(|n| *n > MAX_LIST_ITEMS) {
            return Err(format!("條件項目過多（每項最多 {MAX_LIST_ITEMS} 個）"));
        }
        let texts = self
            .country_codes
            .iter()
            .chain(&self.langs)
            .chain(&self.partner_tags)
            .chain(&self.keywords);
        for t in texts {
            if t.trim().is_empty() || t.chars().count() > 80 {
                return Err("條件值不可空白且最多 80 字".into());
            }
        }
        Ok(())
    }

    /// Whether every criterion of this rule holds for `visitor`.
    pub fn matches(&self, visitor: &VisitorProfile) -> bool {
        if !self.enabled {
            return false;
        }
        let id_ok = |ids: &[i64], v: Option<i64>| {
            ids.is_empty() || v.is_some_and(|v| ids.contains(&v))
        };
        if !id_ok(&self.livechat_channel_ids, visitor.livechat_channel_id)
            || !id_ok(&self.chatbot_script_ids, visitor.chatbot_script_id)
        {
            return false;
        }
        if !self.country_codes.is_empty() {
            let Some(code) = visitor.country_code.as_deref() else {
                return false;
            };
            if !self
                .country_codes
                .iter()
                .any(|c| c.trim().eq_ignore_ascii_case(code))
            {
                return false;
            }
        }
        if !self.langs.is_empty() {
            let Some(lang) = visitor.lang.as_deref() else {
                return false;
            };
            if !self.langs.iter().any(|l| lang_matches(l, lang)) {
                return false;
            }
        }
        match self.visitor {
            VisitorKind::Any => {}
            VisitorKind::LoggedIn if !visitor.logged_in => return false,
            VisitorKind::Anonymous if visitor.logged_in => return false,
            _ => {}
        }
        if !self.partner_tags.is_empty()
            && !self.partner_tags.iter().any(|want| {
                visitor
                    .partner_tags
                    .iter()
                    .any(|have| have.trim().to_lowercase() == want.trim().to_lowercase())
            })
        {
            return false;
        }
        if !self.keywords.is_empty()
            && !self.keywords.iter().any(|k| {
                duduclaw_core::word_contains_ci(&visitor.visitor_text, k.trim())
            })
        {
            return false;
        }
        true
    }
}

/// `want` = rule value, `have` = visitor's Odoo lang code (`zh_TW`).
fn lang_matches(want: &str, have: &str) -> bool {
    let want = want.trim().replace('-', "_");
    let have = have.trim().replace('-', "_");
    if want.is_empty() {
        return false;
    }
    if want.eq_ignore_ascii_case(&have) {
        return true;
    }
    !want.contains('_')
        && have
            .split('_')
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case(&want))
}

/// What the bridge knows about a visitor when routing a session.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VisitorProfile {
    pub livechat_channel_id: Option<i64>,
    pub chatbot_script_id: Option<i64>,
    pub country_code: Option<String>,
    pub lang: Option<String>,
    pub logged_in: bool,
    pub partner_tags: Vec<String>,
    /// The visitor's messages so far, joined by newlines (keyword matching).
    #[serde(skip)]
    pub visitor_text: String,
}

/// Outcome of routing one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteDecision {
    /// Configured agent name; empty ⇒ the gateway's main agent.
    pub agent: String,
    /// Matched rule name, `None` when the default applied.
    pub rule: Option<String>,
}

/// First enabled rule that matches wins; otherwise `default_agent`.
pub fn route(cfg: &LivechatConfig, visitor: &VisitorProfile) -> RouteDecision {
    match cfg.audience_rules.iter().find(|r| r.matches(visitor)) {
        Some(rule) => RouteDecision {
            agent: rule.agent.trim().to_string(),
            rule: Some(rule.name.trim().to_string()),
        },
        None => RouteDecision {
            agent: cfg.default_agent.trim().to_string(),
            rule: None,
        },
    }
}

/// Whether the visitor's text asks for a human.
pub fn is_handoff_request(text: &str, keywords: &[String]) -> bool {
    keywords
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .any(|k| duduclaw_core::word_contains_ci(text, k))
}

// ── Message text ────────────────────────────────────────────────

/// Odoo message HTML → plain text. Block ends become newlines, tags are
/// dropped, the common entities are decoded. Not a sanitizer: the output is
/// only ever fed to a model as text.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let mut tag = String::new();
        for t in chars.by_ref() {
            if t == '>' {
                break;
            }
            tag.push(t);
        }
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let closing = tag.starts_with('/');
        let breaks = matches!(name.as_str(), "br")
            || (closing && matches!(name.as_str(), "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3"));
        if breaks {
            out.push('\n');
        }
    }
    let decoded = decode_entities(&out);
    let lines: Vec<&str> = decoded.lines().map(str::trim_end).collect();
    lines.join("\n").trim().to_string()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // Entities are short; look for `;` within the next few chars only
        // (by char, never by raw byte index — the text is CJK-heavy).
        let semicolon = tail
            .char_indices()
            .take(12)
            .find(|(_, ch)| *ch == ';')
            .map(|(i, _)| i);
        let Some(end) = semicolon else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" | "#x27" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Light Markdown cleanup for a widget that renders neither Markdown nor
/// arbitrary HTML: drop emphasis markers and heading hashes, turn list
/// dashes into bullets, unwrap fenced code.
pub fn markdown_to_plain(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            continue;
        }
        let indent = &line[..line.len() - trimmed.len()];
        let body = if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            format!("• {rest}")
        } else if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim_start().to_string()
        } else {
            trimmed.to_string()
        };
        let cleaned = body.replace("**", "").replace("__", "").replace('`', "");
        lines.push(format!("{indent}{cleaned}"));
    }
    lines.join("\n").trim().to_string()
}

/// Agent reply → safe HTML for `message_post(body_is_html=True)`. Everything
/// is escaped first, so the only markup that reaches Odoo is the `<br>` this
/// function adds.
pub fn reply_to_html(text: &str) -> String {
    let plain = markdown_to_plain(text);
    plain
        .lines()
        .map(escape_html)
        .collect::<Vec<_>>()
        .join("<br>")
}

#[cfg(test)]
mod tests;
