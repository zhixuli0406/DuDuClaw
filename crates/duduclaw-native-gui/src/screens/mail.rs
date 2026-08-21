// WP-S5b3-I (2026-08-21) — 信箱 (`Mail.dc.html`, B15, Agent Mail / P2-d).
// Two-tab 收件匣/待寄出 master-detail — mirrors `web/src/pages/MailPage.tsx`'s
// own framing (「收件匣是讀取面、待寄出是決策面」) inside this crate's
// established list-column + detail-column shape.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed; all six gated `require_manager!()`) ─────────
//   `mail.status {}` (`handle_mail_status`, ~L31559) → `{ "enabled",
//     "auto_trigger","gmail_enabled","dropfolder_enabled",
//     "poll_interval_secs","default_agent","smtp_configured",
//     "sender_allowlist_count","recipient_allowlist_count","inbound_dir" }`.
//   `mail.list {"agent_id"?,"include_archived"?,"limit"?}`
//     (`handle_mail_list`, ~L31586) → `{ "count", "messages": [ {
//     "mail_id","agent_id","from","subject","snippet","received_at",
//     "source","read","archived","handled","flagged","risk_score" } ] }`.
//   `mail.read {"mail_id"}` (`handle_mail_read`, ~L31643) → same shape plus
//     "body" (no "snippet") — also marks the message read server-side.
//   `mail.outbox {"agent_id"?,"status"?,"limit"?}` (`handle_mail_outbox`,
//     ~L31705) → `{ "count", "drafts": [ { "mail_id","agent_id","to",
//     "subject","body","created_at","status" ("pending"|"sent"|"rejected"|
//     "failed"),"approval_id","in_reply_to","note","settled_at",
//     "decision_note" } ] }`.
//   `mail.archive {"mail_id"}` / `mail.decide {"mail_id","approve","note"?}`
//     exist (`handle_mail_archive` ~L31684 / `handle_mail_decide` ~L31768)
//     but are NOT called by this pass — see deviation §1 below.
//
// ── Canvas fidelity deviations (documented, not silent) ───────────────────
// 1. "確認寄出" / "不要寄" — assembled, not wired. This WP's own brief states
//    the product rule explicitly: mail leaving the building is an
//    ApprovalBroker decision, never a dashboard click this pass adds new
//    plumbing for. Same "write path assembled not wired" convention `plans.
//    rs`'s "新增步驟" button / `mcp_keys.rs`'s create-revoke buttons already
//    establish — both buttons render, neither calls `mail.decide`.
// 2. "撰寫" (compose) — assembled, not wired (this WP's own brief: "研究未
//    覆蓋，誠實佔位"). No compose dialog exists in this crate yet.
// 3. 封存 ("archive") on an inbox row — also left unwired this pass, same
//    reasoning as §1 (decluttering the inbox is a smaller-stakes action than
//    sending, but this pass keeps every write path off, not a mix of some
//    on/some off, to keep the "which buttons are real" story simple and
//    auditable in one place).
// 4. Reading a message (`mail.read`, which marks it read server-side as a
//    side effect) IS wired — selecting a row is this crate's established
//    master-detail convention everywhere else (`plans.rs`'s `plans.get`,
//    `canvas.rs`'s `canvas.get` on selection change), and `mail.read`'s own
//    "read" side effect is the same one-way, safe-to-repeat status flip
//    every other page's selection-triggered detail fetch already causes.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::screens::goals::relative_time;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MailStatus {
    pub enabled: bool,
    pub smtp_configured: bool,
}

pub fn parse_mail_status(v: &Value) -> MailStatus {
    MailStatus {
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        smtp_configured: v.get("smtp_configured").and_then(Value::as_bool).unwrap_or(false),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MailMessage {
    pub mail_id: String,
    pub from: String,
    pub subject: String,
    pub snippet: String,
    pub received_at: String,
    pub source: String,
    pub read: bool,
    pub handled: bool,
    pub flagged: bool,
}

pub fn parse_mail_messages(v: &Value) -> Vec<MailMessage> {
    v.get("messages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(MailMessage {
                        mail_id: m.get("mail_id").and_then(Value::as_str)?.to_string(),
                        from: m.get("from").and_then(Value::as_str).unwrap_or("").to_string(),
                        subject: m.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
                        snippet: m.get("snippet").and_then(Value::as_str).unwrap_or("").to_string(),
                        received_at: m.get("received_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        source: m.get("source").and_then(Value::as_str).unwrap_or("").to_string(),
                        read: m.get("read").and_then(Value::as_bool).unwrap_or(false),
                        handled: m.get("handled").and_then(Value::as_bool).unwrap_or(false),
                        flagged: m.get("flagged").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MailMessageFull {
    pub subject: String,
    pub from: String,
    pub received_at: String,
    pub source: String,
    pub body: String,
    pub flagged: bool,
}

pub fn parse_mail_message_full(v: &Value) -> MailMessageFull {
    MailMessageFull {
        subject: v.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
        from: v.get("from").and_then(Value::as_str).unwrap_or("").to_string(),
        received_at: v.get("received_at").and_then(Value::as_str).unwrap_or("").to_string(),
        source: v.get("source").and_then(Value::as_str).unwrap_or("").to_string(),
        body: v.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
        flagged: v.get("flagged").and_then(Value::as_bool).unwrap_or(false),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MailDraft {
    pub mail_id: String,
    pub agent_id: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub created_at: String,
    pub status: String,
}

pub fn parse_mail_drafts(v: &Value) -> Vec<MailDraft> {
    v.get("drafts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    Some(MailDraft {
                        mail_id: d.get("mail_id").and_then(Value::as_str)?.to_string(),
                        agent_id: d.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        to: d.get("to").and_then(Value::as_str).unwrap_or("").to_string(),
                        subject: d.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
                        body: d.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
                        created_at: d.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        status: d.get("status").and_then(Value::as_str).unwrap_or("pending").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailTab {
    Inbox,
    Outbox,
}

impl MailTab {
    fn label_key(self) -> &'static str {
        match self {
            MailTab::Inbox => "mail.tab.inbox",
            MailTab::Outbox => "mail.tab.outbox",
        }
    }
}

/// First line, CJK-safe char-truncated (coding convention #1 — never a raw
/// byte slice).
fn summarize(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

// ── Global state ───────────────────────────────────────────────────────

pub struct MailState {
    requested: bool,
    pub status: Loadable<MailStatus>,
    pub inbox: Loadable<Vec<MailMessage>>,
    pub outbox: Loadable<Vec<MailDraft>>,
    pub tab: MailTab,
    pub selected_inbox: Option<String>,
    pub selected_outbox: Option<String>,
    read_for: Option<String>,
    pub inbox_detail: Loadable<MailMessageFull>,
}

impl MailState {
    fn new() -> Self {
        Self {
            requested: false,
            status: Loadable::Loading,
            inbox: Loadable::Loading,
            outbox: Loadable::Loading,
            tab: MailTab::Inbox,
            selected_inbox: None,
            selected_outbox: None,
            read_for: None,
            inbox_detail: Loadable::Loading,
        }
    }
}

impl Global for MailState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<MailState>() {
        cx.set_global(MailState::new());
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<MailState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<MailState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "mail.status", json!({}), |cx, result| {
        cx.global_mut::<MailState>().status = result.map(|v| parse_mail_status(&v)).into();
    });
    spawn_call(cx, tx.clone(), "mail.list", json!({ "limit": 50 }), |cx, result| {
        cx.global_mut::<MailState>().inbox = result.map(|v| parse_mail_messages(&v)).into();
    });
    spawn_call(cx, tx, "mail.outbox", json!({ "limit": 50 }), |cx, result| {
        cx.global_mut::<MailState>().outbox = result.map(|v| parse_mail_drafts(&v)).into();
    });
}

fn maybe_fetch_inbox_detail(state: &RootView, cx: &mut Context<RootView>, mail_id: &str) {
    if cx.global::<MailState>().read_for.as_deref() == Some(mail_id) {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    {
        let g = cx.global_mut::<MailState>();
        g.read_for = Some(mail_id.to_string());
        g.inbox_detail = Loadable::Loading;
    }
    let tx = state.session_tx.clone();
    let key = mail_id.to_string();
    spawn_call(cx, tx, "mail.read", json!({ "mail_id": mail_id }), move |cx, result| {
        if cx.global::<MailState>().read_for.as_deref() != Some(key.as_str()) {
            return;
        }
        let g = cx.global_mut::<MailState>();
        g.inbox_detail = result.map(|v| parse_mail_message_full(&v)).into();
        // The RPC's own "read" side effect — reflect it locally without a
        // full `mail.list` refetch, same "optimistic local flip" precedent
        // `inbox.rs::dispatch_decide`'s own doc comment documents.
        if let Loadable::Ready(rows) = &mut g.inbox {
            if let Some(m) = rows.iter_mut().find(|m| m.mail_id == key) {
                m.read = true;
            }
        }
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Tabs ────────────────────────────────────────────────────────────

fn tab_pill(tab: MailTab, count: usize, active: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let row_id: SharedString = format!("mail-tab-{}", tab.label_key()).into();
    let label = i18n::t1(locale, tab.label_key(), "n", &count.to_string());
    div()
        .id(row_id)
        .px_3p5()
        .py_1p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::MEDIUM })
        .bg(theme::alpha(theme::SURFACE, if active { 1.0 } else { 0.0 }))
        .text_color(if active { theme::alpha(theme::BRAND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MailState>().tab = tab;
            cx.notify();
        }))
}

// ── Inbox ───────────────────────────────────────────────────────────

fn inbox_row(m: &MailMessage, selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let when = relative_time(locale, &m.received_at, chrono::Utc::now());
    let row_id: SharedString = format!("mail-inbox-{}", m.mail_id).into();
    let id_for_click = m.mail_id.clone();

    div()
        .id(row_id)
        .flex()
        .flex_col()
        .gap_1()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .bg(if selected { theme::alpha(theme::SIDEBAR_ACCENT, 1.0) } else { theme::alpha(theme::SURFACE, 1.0) })
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(if m.read { gpui::FontWeight::NORMAL } else { gpui::FontWeight::SEMIBOLD })
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(if m.from.is_empty() { i18n::t(locale, "mail.unknownSender") } else { m.from.clone().into() }),
                )
                .child(div().flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(when)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .overflow_hidden()
                .child(if m.subject.is_empty() { i18n::t(locale, "mail.noSubject") } else { m.subject.clone().into() }),
        )
        .children((!m.snippet.is_empty()).then(|| {
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.85))
                .overflow_hidden()
                .child(summarize(&m.snippet, 90))
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .children((!m.read).then(|| badge(i18n::t(locale, "mail.badge.unread"), BadgeVariant::Default)))
                .children(m.flagged.then(|| badge(i18n::t(locale, "mail.badge.flagged"), BadgeVariant::Destructive)))
                .children(m.handled.then(|| badge(i18n::t(locale, "mail.badge.handled"), BadgeVariant::Secondary))),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MailState>().selected_inbox = Some(id_for_click.clone());
            cx.notify();
        }))
}

fn inbox_detail(detail: &Loadable<MailMessageFull>, locale: Locale) -> Stateful<Div> {
    // `.overflow_y_scroll()` lives on `StatefulInteractiveElement` (gpui's
    // scroll position needs a stable identity to track across renders) —
    // `.id(...)` first, same "scrollable panel needs an id" requirement
    // every other scrollable column in this crate already satisfies.
    let panel = div().id("mail-inbox-detail").flex_1().min_w_0().overflow_y_scroll().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).p_4();

    match detail {
        Loadable::Loading => panel.flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "mail.loading"), None, None::<Div>)),
        Loadable::Failed(e) => panel.flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "mail.loadError", "message", e), None, None::<Div>)),
        Loadable::Ready(m) => {
            let mut col = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(if m.subject.is_empty() { i18n::t(locale, "mail.noSubject") } else { m.subject.clone().into() }))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::tn(locale, "mail.meta", &[("from", &m.from), ("at", &m.received_at), ("source", &m.source)])),
                );
            if m.flagged {
                col = col.child(
                    div()
                        .p_2p5()
                        .rounded(px(theme::RADIUS_MD))
                        .bg(theme::alpha(theme::DESTRUCTIVE, 0.08))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                        .child(i18n::t(locale, "mail.flaggedNotice")),
                );
            }
            col = col.child(
                div()
                    .mt_2()
                    .p_3()
                    .rounded(px(theme::RADIUS_LG))
                    .bg(theme::alpha(theme::MUTED, 0.4))
                    .text_size(px(theme::TEXT_SM))
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .child(m.body.clone()),
            );
            panel.child(col)
        }
    }
}

fn inbox_tab(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let inbox = cx.global::<MailState>().inbox.clone();

    match &inbox {
        Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "mail.loading"), None, None::<Div>)),
        Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "mail.loadError", "message", e), None, None::<Div>)),
        Loadable::Ready(rows) if rows.is_empty() => div().flex_1().flex().items_center().justify_center().child(empty_state("📭", i18n::t(locale, "mail.inbox.empty"), Some(i18n::t(locale, "mail.inbox.empty.desc")), None::<Div>)),
        Loadable::Ready(rows) => {
            let selected = cx.global::<MailState>().selected_inbox.clone().or_else(|| rows.first().map(|m| m.mail_id.clone()));
            if let Some(id) = &selected {
                maybe_fetch_inbox_detail(state, cx, id);
            }
            let mut list = div().id("mail-inbox-list").w(px(320.)).flex_shrink_0().overflow_y_scroll().flex().flex_col().gap_1p5();
            for m in rows {
                list = list.child(inbox_row(m, selected.as_deref() == Some(m.mail_id.as_str()), locale, cx));
            }
            let detail = cx.global::<MailState>().inbox_detail.clone();
            div().flex_1().min_h_0().flex().gap_3().child(list).child(inbox_detail(&detail, locale))
        }
    }
}

// ── Outbox ──────────────────────────────────────────────────────────

fn draft_status_badge(status: &str, locale: Locale) -> Div {
    let (label_key, variant) = match status {
        "sent" => ("mail.status.sent", BadgeVariant::Secondary),
        "failed" => ("mail.status.failed", BadgeVariant::Destructive),
        "rejected" => ("mail.status.rejected", BadgeVariant::Outline),
        _ => ("mail.status.pending", BadgeVariant::Warning),
    };
    badge(i18n::t(locale, label_key), variant)
}

fn outbox_row(d: &MailDraft, selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let when = relative_time(locale, &d.created_at, chrono::Utc::now());
    let row_id: SharedString = format!("mail-outbox-{}", d.mail_id).into();
    let id_for_click = d.mail_id.clone();

    div()
        .id(row_id)
        .flex()
        .flex_col()
        .gap_1()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .bg(if selected { theme::alpha(theme::SIDEBAR_ACCENT, 1.0) } else { theme::alpha(theme::SURFACE, 1.0) })
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().flex_1().min_w_0().overflow_hidden().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(if d.subject.is_empty() { i18n::t(locale, "mail.noSubject") } else { d.subject.clone().into() }))
                .child(div().flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(when)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(draft_status_badge(&d.status, locale))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "mail.draftedBy", "agent", &d.agent_id))),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MailState>().selected_outbox = Some(id_for_click.clone());
            cx.notify();
        }))
}

fn outbox_detail(draft: Option<&MailDraft>, locale: Locale) -> Stateful<Div> {
    // Same "`.overflow_y_scroll()` needs an id" note as `inbox_detail` above.
    let panel = div().id("mail-outbox-detail").flex_1().min_w_0().overflow_y_scroll().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).p_4();

    let Some(d) = draft else {
        return panel.flex().items_center().justify_center().child(empty_state("👈", i18n::t(locale, "mail.detail.empty"), None, None::<Div>));
    };

    let col = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(if d.subject.is_empty() { i18n::t(locale, "mail.noSubject") } else { d.subject.clone().into() }))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::tn(locale, "mail.draft.meta", &[("to", &d.to), ("agent", &d.agent_id), ("at", &d.created_at)])),
        )
        .child(
            div()
                .p_2p5()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::WARNING, 0.10))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::WARNING, 1.0))
                .child(i18n::t(locale, "mail.outbox.rule")),
        )
        .child(
            div()
                .mt_1()
                .p_3()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::MUTED, 0.4))
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(d.body.clone()),
        );

    let actions = if d.status == "pending" {
        // Assembled, not wired — see this module's header comment §1.
        Some(
            div()
                .mt_2()
                .flex()
                .gap_2()
                .child(button("mail-confirm-send", i18n::t(locale, "mail.action.confirmSend"), ButtonVariant::Primary, false, None, |_ev, _window, _app| {}))
                .child(button("mail-reject", i18n::t(locale, "mail.action.reject"), ButtonVariant::Destructive, false, None, |_ev, _window, _app| {})),
        )
    } else {
        None
    };

    panel.child(col.children(actions))
}

// No server fetch keyed by selection here — `mail.outbox`'s own list
// already carries every draft's full body (unlike `inbox_tab`, which fetches
// `mail.read` per selection since `mail.list` only sends a snippet).
fn outbox_tab(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let outbox = cx.global::<MailState>().outbox.clone();

    match &outbox {
        Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "mail.loading"), None, None::<Div>)),
        Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "mail.loadError", "message", e), None, None::<Div>)),
        Loadable::Ready(rows) if rows.is_empty() => div().flex_1().flex().items_center().justify_center().child(empty_state("📤", i18n::t(locale, "mail.outbox.empty"), Some(i18n::t(locale, "mail.outbox.empty.desc")), None::<Div>)),
        Loadable::Ready(rows) => {
            let selected = cx.global::<MailState>().selected_outbox.clone().or_else(|| rows.first().map(|d| d.mail_id.clone()));
            let mut list = div().id("mail-outbox-list").w(px(320.)).flex_shrink_0().overflow_y_scroll().flex().flex_col().gap_1p5();
            for d in rows {
                list = list.child(outbox_row(d, selected.as_deref() == Some(d.mail_id.as_str()), locale, cx));
            }
            let current = selected.as_deref().and_then(|id| rows.iter().find(|d| d.mail_id == id));
            div().flex_1().min_h_0().flex().gap_3().child(list).child(outbox_detail(current, locale))
        }
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("mail-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let status = cx.global::<MailState>().status.clone();
    let inbox_unread = match &cx.global::<MailState>().inbox {
        Loadable::Ready(rows) => rows.iter().filter(|m| !m.read).count(),
        _ => 0,
    };
    let outbox_pending = match &cx.global::<MailState>().outbox {
        Loadable::Ready(rows) => rows.iter().filter(|d| d.status == "pending").count(),
        _ => 0,
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(div().size(px(30.)).rounded(px(theme::RADIUS_MD)).flex().items_center().justify_center().bg(theme::alpha(theme::INFO, 0.14)).child("✉️"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "mail.title")))
                        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "mail.subtitle"))),
                ),
        )
        // Assembled, not wired — see this module's header comment §2.
        .child(button("mail-compose", i18n::t(locale, "mail.action.compose"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}));

    let disabled_notice = matches!(&status, Loadable::Ready(s) if !s.enabled).then(|| {
        div()
            .p_2p5()
            .rounded(px(theme::RADIUS_MD))
            .bg(theme::alpha(theme::MUTED, 0.5))
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "mail.disabled"))
    });

    let tab = cx.global::<MailState>().tab;
    let tab_row = div()
        .flex()
        .gap_1()
        .p_1()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::MUTED, 0.6))
        .child(tab_pill(MailTab::Inbox, inbox_unread, tab == MailTab::Inbox, locale, cx))
        .child(tab_pill(MailTab::Outbox, outbox_pending, tab == MailTab::Outbox, locale, cx));

    let body: Div = if tab == MailTab::Inbox { inbox_tab(state, cx) } else { outbox_tab(state, cx) };

    div().id("mail-page").size_full().flex().flex_col().gap_3().p_4().child(header).children(disabled_notice).child(tab_row).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mail_messages_skips_rows_without_an_id() {
        let v = json!({ "messages": [
            { "mail_id": "m1", "from": "陳經理", "subject": "S", "snippet": "…", "received_at": "t", "source": "gmail", "read": false, "handled": true, "flagged": false, "risk_score": 0.1 },
            { "from": "no id" },
        ] });
        let rows = parse_mail_messages(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mail_id, "m1");
        assert!(!rows[0].read);
    }

    #[test]
    fn parse_mail_drafts_defaults_status_to_pending() {
        let v = json!({ "drafts": [ { "mail_id": "d1", "agent_id": "dudu", "to": "a@b.com", "subject": "S", "body": "B", "created_at": "t" } ] });
        let rows = parse_mail_drafts(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "pending");
    }

    #[test]
    fn parse_mail_status_reads_enabled_and_smtp() {
        let v = json!({ "enabled": true, "smtp_configured": false });
        let s = parse_mail_status(&v);
        assert!(s.enabled);
        assert!(!s.smtp_configured);
    }

    #[test]
    fn summarize_appends_ellipsis_only_when_truncated() {
        assert_eq!(summarize("嘟嘟事務所", 10), "嘟嘟事務所");
        assert_eq!(summarize("嘟嘟事務所的月報", 4), "嘟嘟事務…");
    }

    #[test]
    fn parse_mail_message_full_reads_body() {
        let v = json!({ "subject": "S", "from": "a", "received_at": "t", "source": "gmail", "body": "hello", "flagged": true });
        let m = parse_mail_message_full(&v);
        assert_eq!(m.body, "hello");
        assert!(m.flagged);
    }
}
