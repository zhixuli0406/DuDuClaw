//! One AI turn in one live chat session: greet, hand off, route, answer.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use duduclaw_odoo::livechat::rpc::{self, ChatMessage, Session};
use duduclaw_odoo::livechat::{
    html_to_text, is_handoff_request, reply_to_html, route, LivechatConfig, VisitorProfile,
};
use duduclaw_odoo::OdooConnector;
use tracing::{info, warn};

use super::{classify, read_state, with_state, Author};
use crate::channel_reply::{build_guarded_reply_for_agent, ReplyContext};

pub(super) struct TurnInput {
    /// [`super::instance_tag`] of the Odoo the session lives in.
    pub instance: String,
    pub session: Session,
    /// Unanswered visitor messages, oldest first. May be empty when the turn
    /// only greets.
    pub visitor: Vec<ChatMessage>,
    /// Chatbot-script transcript before `visitor` (first sight only).
    pub earlier: Vec<(Author, ChatMessage)>,
    pub greet: bool,
    pub bot_partner_id: i64,
}

pub(super) async fn run(
    home: &Path,
    ctx: &Arc<ReplyContext>,
    conn: &OdooConnector,
    lc: &LivechatConfig,
    input: TurnInput,
    internal: &HashSet<i64>,
) {
    let sid = input.session.id;
    if input.greet {
        let greeting = lc.greeting.trim();
        match rpc::post_reply(conn, sid, &reply_to_html(greeting), greeting).await {
            Ok(_) => with_state(home, |st| {
                if let Some(s) = st.sessions.get_mut(&sid) {
                    s.greeted = true;
                }
            }),
            Err(e) => warn!(session = sid, error = %e, "odoo livechat: greeting failed"),
        }
    }
    let Some(first) = input.visitor.first() else {
        return;
    };
    let text = input
        .visitor
        .iter()
        .map(|m| html_to_text(&m.body_html))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return;
    }
    // Instance-tagged: partner and guest ids restart in every database, and
    // per-user memory must never carry over to another Odoo's visitor.
    let principal = match (first.author_partner_id, first.author_guest_id) {
        (Some(p), _) => format!("odoo-{}-partner-{p}", input.instance),
        (None, Some(g)) => format!("odoo-{}-guest-{g}", input.instance),
        (None, None) => String::new(),
    };

    if is_handoff_request(&text, &lc.handoff_keywords) && !lc.handoff_user_ids.is_empty() {
        match rpc::invite_users(conn, sid, &lc.handoff_user_ids).await {
            Ok(n) if n > 0 => {
                let msg = lc.handoff_message.trim();
                if !msg.is_empty() {
                    if let Err(e) = rpc::post_reply(conn, sid, &reply_to_html(msg), msg).await {
                        warn!(session = sid, error = %e, "odoo livechat: handoff notice failed");
                    }
                }
                with_state(home, |st| {
                    if let Some(s) = st.sessions.get_mut(&sid) {
                        s.human = true;
                        s.human_reason = Some("visitor_requested".into());
                    }
                });
                info!(session = sid, invited = n, "odoo livechat: handed off to human operators");
                return;
            }
            Ok(_) => warn!(session = sid, "odoo livechat: handoff users not found — the AI keeps answering"),
            Err(e) => warn!(session = sid, error = %e, "odoo livechat: handoff invite failed — the AI keeps answering"),
        }
    }

    let (agent, first_turn) = ensure_routed(home, ctx, conn, lc, &input, &text).await;
    let Some(agent) = agent else {
        // A configured employee that is not loaded (renamed, removed) is not
        // silently replaced by another one: the visitor gets the generic
        // apology and the dashboard shows why.
        warn!(session = sid, "odoo livechat: no AI employee available for this visitor");
        super::update_status(|s| {
            s.message = "分眾規則或預設值指定的 AI 員工不存在，訪客訊息未由 AI 回覆".to_string();
            s.last_error_at = Some(chrono::Utc::now().to_rfc3339());
        });
        let msg = super::VISITOR_FALLBACK_MESSAGE;
        if let Err(e) = rpc::post_reply(conn, sid, &reply_to_html(msg), msg).await {
            warn!(session = sid, error = %e, "odoo livechat: posting the fallback failed");
        }
        return;
    };
    let prompt = if first_turn {
        let mut p = context_line(home, sid);
        let transcript = chatbot_transcript(&input.earlier);
        if !transcript.is_empty() {
            p.push_str("\n[訪客先前與聊天機器人的對話]\n");
            p.push_str(&transcript);
            p.push_str("\n[訪客的新訊息]");
        }
        format!("{p}\n{text}")
    } else {
        text
    };
    let session_key = super::session_key(&input.instance, sid);
    let guarded =
        build_guarded_reply_for_agent(&prompt, ctx, &agent, &session_key, &principal, None).await;
    if !guarded.still_valid().await || guarded.text.trim().is_empty() {
        warn!(session = sid, "odoo livechat: reply withdrawn or empty — nothing posted");
        return;
    }

    // A human may have stepped in while the AI was thinking: do not talk
    // over them.
    if operator_joined_since(conn, sid, input.bot_partner_id, internal, home).await {
        info!(session = sid, "odoo livechat: operator spoke during generation — reply dropped");
        return;
    }
    let reply = guarded.text.trim();
    match rpc::post_reply(conn, sid, &reply_to_html(reply), reply).await {
        Ok(_) => with_state(home, |st| {
            if let Some(s) = st.sessions.get_mut(&sid) {
                s.replies += 1;
                s.context_sent = true;
                s.last_activity = chrono::Utc::now().to_rfc3339();
            }
        }),
        Err(e) => warn!(session = sid, error = %e, "odoo livechat: posting the reply failed"),
    }
}

/// Route the session on its first visitor message (sticky afterwards).
/// Returns the agent to use and whether this is the session's first AI turn.
async fn ensure_routed(
    home: &Path,
    ctx: &Arc<ReplyContext>,
    conn: &OdooConnector,
    lc: &LivechatConfig,
    input: &TurnInput,
    visitor_text: &str,
) -> (Option<String>, bool) {
    let sid = input.session.id;
    let (assigned, context_sent) = read_state(home, |st| {
        st.sessions
            .get(&sid)
            .map(|s| (s.agent.clone(), s.context_sent))
            .unwrap_or_default()
    });
    if !assigned.is_empty() && agent_exists(ctx, &assigned).await {
        return (Some(assigned), !context_sent);
    }
    let first = &input.visitor[0];
    let mut countries = HashMap::new();
    let facts = rpc::visitor_facts(
        conn,
        &input.session,
        first.author_partner_id,
        first.author_guest_id,
        &mut countries,
    )
    .await;
    let profile = VisitorProfile {
        livechat_channel_id: input.session.livechat_channel_id,
        chatbot_script_id: facts.chatbot_script_id,
        country_code: facts.country_code.clone(),
        lang: facts.lang.clone(),
        logged_in: facts.logged_in,
        partner_tags: facts.partner_tags.clone(),
        visitor_text: visitor_text.to_string(),
    };
    let decision = route(lc, &profile);
    let agent = resolve_agent(ctx, &decision.agent).await;
    if let Some(a) = &agent {
        info!(
            session = sid,
            agent = %a,
            rule = decision.rule.as_deref().unwrap_or("(default)"),
            "odoo livechat: session routed"
        );
        with_state(home, |st| {
            if let Some(s) = st.sessions.get_mut(&sid) {
                s.agent = a.clone();
                s.rule = decision.rule.clone();
                s.country = facts.country_code.clone();
                s.lang = facts.lang.clone();
                s.logged_in = facts.logged_in;
            }
        });
    }
    (agent, !context_sent)
}

async fn agent_exists(ctx: &Arc<ReplyContext>, name: &str) -> bool {
    ctx.registry.read().await.get(name).is_some()
}

/// The configured agent if it is loaded; the main agent only when nothing
/// was configured. A configured name that is not loaded answers `None`
/// (fail closed): a public visitor must not land on an employee the admin
/// never chose for them.
async fn resolve_agent(ctx: &Arc<ReplyContext>, configured: &str) -> Option<String> {
    let reg = ctx.registry.read().await;
    if configured.is_empty() {
        return reg.main_agent().map(|a| a.config.agent.name.clone());
    }
    if reg.get(configured).is_some() {
        return Some(configured.to_string());
    }
    warn!(agent = %configured, "odoo livechat: configured AI employee is not loaded");
    None
}

/// One line of context for the first turn, so the employee knows it is on
/// the company website's live chat and who it is talking to. Built only
/// from Odoo facts, never from visitor text.
fn context_line(home: &Path, sid: i64) -> String {
    read_state(home, |st| {
        let Some(s) = st.sessions.get(&sid) else {
            return "[Odoo 網站線上客服]".to_string();
        };
        let mut parts = vec!["Odoo 網站線上客服".to_string()];
        if !s.visitor_name.is_empty() {
            parts.push(format!("訪客：{}", s.visitor_name));
        }
        parts.push(if s.logged_in { "已登入會員" } else { "未登入訪客" }.to_string());
        if let Some(c) = &s.country {
            parts.push(format!("國家：{c}"));
        }
        if let Some(l) = &s.lang {
            parts.push(format!("語言：{l}"));
        }
        if let Some(r) = &s.rule {
            parts.push(format!("分眾：{r}"));
        }
        format!("[{}]", parts.join("｜"))
    })
}

/// `聊天機器人：…` / `訪客：…` lines, oldest first, capped so a long
/// script cannot crowd out the actual question.
fn chatbot_transcript(earlier: &[(Author, ChatMessage)]) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<String> = earlier
        .iter()
        .filter_map(|(who, m)| {
            let text = html_to_text(&m.body_html);
            if text.is_empty() {
                return None;
            }
            let label = if *who == Author::Visitor { "訪客" } else { "聊天機器人" };
            Some(format!("{label}：{}", duduclaw_core::truncate_chars(&text, 300)))
        })
        .collect();
    let skip = lines.len().saturating_sub(MAX_LINES);
    lines[skip..].join("\n")
}

/// Whether an internal user other than the bot posted after the cursor.
/// Read errors answer `false`: the reply then goes out, which is the same
/// outcome as a human who spoke a moment after it.
async fn operator_joined_since(
    conn: &OdooConnector,
    sid: i64,
    bot_partner_id: i64,
    internal: &HashSet<i64>,
    home: &Path,
) -> bool {
    let cursor = read_state(home, |st| st.sessions.get(&sid).map(|s| s.last_id).unwrap_or(0));
    let Ok(msgs) = rpc::messages_since(conn, &[(sid, cursor)]).await else {
        return false;
    };
    let unknown: Vec<i64> = msgs
        .iter()
        .filter_map(|m| m.author_partner_id)
        .filter(|p| *p != bot_partner_id && !internal.contains(p))
        .collect();
    let mut internal = internal.clone();
    if !unknown.is_empty() {
        if let Ok(more) = rpc::internal_partner_ids(conn, &unknown).await {
            internal.extend(more);
        }
    }
    let empty = HashSet::new();
    msgs.iter()
        .any(|m| classify(m, bot_partner_id, &internal, &empty) == Author::Operator)
}
