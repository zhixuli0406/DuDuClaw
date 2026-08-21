// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "知識審核" (`KnowledgeCuration.dc.
// html`, B25+頁型3). Still no `nav.rs` id of its own — self-attached in
// `screens/shell.rs`, reached from `knowledge_hub.rs`'s own 5-tab strip's
// 審核 tab (`knowledge_common::shell_tabs`) or
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=knowledgeCuration` directly. WP-S6b3-fix
// (2026-08-22) maps this page's sidebar highlight onto the `knowledgeHub`
// row it drills down from instead — see `knowledge_common.rs`'s module doc
// comment for the fix and `nav::sidebar_active_id`'s own doc comment for
// the mapping.
//
// Visual authority: `KnowledgeCuration.dc.html` — same 5-tab strip as
// `KnowledgeHub.dc.html` (審核 active) → type filter chips (全部/SOP/政策/
// 規格) → a single feed of cards, each expandable to a 核准歸檔/退回 button
// pair. Functional reference only (per this task's "版面禁抄 web"): `web/
// src/pages/KnowledgeCuration.tsx`'s `AutoPagesTab` (this page's 自動建檔
// tab is the ONLY one of its three web sub-tabs this pass ports — the SPO
// 知識圖譜 graph tab and 事實歷史 supersession timeline are out of scope,
// same "圖譜 spike 留後" boundary `knowledge_hub.rs`'s own 圖譜 tab draws).
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/handlers.
// rs` and `auto_wiki_page.rs`, never guessed) ────────────────────────────
//   `agents.list {}` — same default-first-agent fallback every sibling page
//   in this batch uses (no visible agent picker in the canvas).
//   `wiki.auto_pages` (~L14101, `handle_wiki_auto_pages`) — params
//   `{agent_id}` → `{"pages":[AutoPageRow],"exists"}` where `AutoPageRow`
//   (`crates/duduclaw-gateway/src/auto_wiki_page.rs:540`) is `{path,title,
//   updated,doc_type,doc_type_label,sources:[String],revision_count,
//   trust}`. `doc_type` ∈ {charter,sop,spec,policy,reference} (`DocType::
//   ALL`, `knowledge_route.rs:117`); `doc_type_label` is the SERVER's own
//   zh-TW label (章程/流程/規格/政策/參考, `knowledge_route.rs:106`) — note
//   this does NOT literally say "SOP" the way the canvas's badge text does
//   (the real sop label is "流程"); this page renders the real field, not
//   the canvas's illustrative English abbreviation (honest-data-over-canvas-
//   literal-text, same priority the wiki_trust.rs precedent already sets
//   for its own trust-bar color scheme, just the other direction here).
//   `wiki.read` (~L14265) — params `{agent_id,page_path}` → `{"content"}`,
//   fetched on row-expand for a REAL content preview (mirrors `AutoPagesTab
//   `'s own `view()` handler exactly — this is genuine data, not a
//   fabricated excerpt).
//   `wiki.promote`/`wiki.archive` (~L14117/14141) exist and are real, but
//   this task's own brief says "雙鈕展開（核准/退回組裝不真按）" — both
//   buttons render via `mds_gpui::button(..., disabled: true, ...)`, same
//   decision-class-not-wired idiom every other page in this wave uses.
//
// ── Filter chips (real, client-side) ──────────────────────────────────────
// 全部/SOP/政策/規格 map to `doc_type` ∈ {"", "sop", "policy", "spec"} — the
// canvas's own 3 named categories, a subset of the 5 real `DocType::ALL`
// values (章程/參考 pages still show up under 全部, never hidden — a filter
// narrows, it never drops a doc_type from existence).
//
// ── Source formatting (real, not fabricated) ──────────────────────────────
// `format_source` below ports `formatSource()` (`web/src/pages/
// KnowledgeCuration.tsx:773`) verbatim: `AutoPageRow.sources` entries are
// literal strings like `"conversation:telegram:12345:2026-08-04T10:12:33Z"`
// — this turns the last one into `"Telegram · 8/4 10:12"`, matching the
// canvas's own illustrative "阿明 · Slack 對話" STYLE (channel · time) without
// inventing the specific human name the canvas's mockup shows (this RPC
// carries no author-of-source field at all).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::catalog_common::spawn_call;
use crate::screens::dashboard::{error_row, Loadable};
use crate::screens::knowledge_common as kc;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct AutoPageRow {
    pub path: String,
    pub title: String,
    pub updated: String,
    pub doc_type: String,
    pub doc_type_label: String,
    pub sources: Vec<String>,
    pub revision_count: i64,
    /// `trust` (`f32`) is part of the real shape but this canvas draws no
    /// trust indicator on this page (that lives on `WikiTrustPage`) — kept
    /// unused rather than dropped from the struct, so a future pass adding
    /// one doesn't need to re-plumb the parser.
    #[allow(dead_code)]
    pub trust: f32,
}

pub fn parse_auto_pages(v: &Value) -> (Vec<AutoPageRow>, bool) {
    let rows = v
        .get("pages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(AutoPageRow {
                        path: r.get("path")?.as_str()?.to_string(),
                        title: r.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
                        updated: r.get("updated").and_then(Value::as_str).unwrap_or_default().to_string(),
                        doc_type: r.get("doc_type").and_then(Value::as_str).unwrap_or_default().to_string(),
                        doc_type_label: r.get("doc_type_label").and_then(Value::as_str).unwrap_or_default().to_string(),
                        sources: r
                            .get("sources")
                            .and_then(Value::as_array)
                            .map(|s| s.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        revision_count: r.get("revision_count").and_then(Value::as_i64).unwrap_or(1),
                        trust: r.get("trust").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let exists = v.get("exists").and_then(Value::as_bool).unwrap_or(false);
    (rows, exists)
}

const CHANNEL_LABELS: &[(&str, &str)] = &[
    ("telegram", "Telegram"),
    ("discord", "Discord"),
    ("slack", "Slack"),
    ("line", "LINE"),
    ("whatsapp", "WhatsApp"),
    ("feishu", "飛書"),
    ("googlechat", "Google Chat"),
    ("msteams", "Microsoft Teams"),
    ("teams", "Microsoft Teams"),
    ("wecom", "企業微信"),
    ("dingtalk", "釘釘"),
    ("email", "Email"),
    ("webchat", "網頁對話"),
];

/// Finds the byte offset of the first `YYYY-MM-DDT` shape in `s`, scanning
/// every candidate start position (NOT just the first digit — an opaque id
/// segment like `"12345:"` starts with a digit too and must be skipped, not
/// mistaken for the timestamp). Checked purely against raw bytes (never a
/// `&str` slice) so an out-of-bounds or mid-multibyte-char window can never
/// panic — coding convention #1's "never slice strings by raw byte index"
/// concern doesn't apply to byte-array indexing, only to `&str` slicing, and
/// the one `&str` slice this function's caller takes afterwards starts at an
/// already-confirmed ASCII-digit byte, which is always a valid UTF-8
/// boundary.
fn find_iso_timestamp_start(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.len() < 11 {
        return None;
    }
    let is_digit = |b: u8| b.is_ascii_digit();
    for start in 0..=(bytes.len() - 11) {
        let w = &bytes[start..start + 11];
        if is_digit(w[0]) && is_digit(w[1]) && is_digit(w[2]) && is_digit(w[3]) && w[4] == b'-' && is_digit(w[5]) && is_digit(w[6]) && w[7] == b'-' && is_digit(w[8]) && is_digit(w[9]) && w[10] == b'T' {
            return Some(start);
        }
    }
    None
}

/// `conversation:telegram:12345:2026-08-04T10:12:33Z` → `Telegram · 8/4
/// 10:12`. Ports `formatSource()` (see module doc comment) — anything
/// unfamiliar passes through unchanged, never panics (an audit-screen
/// display helper, same contract the web original documents).
pub fn format_source(source: &str) -> String {
    let parts: Vec<&str> = source.split(':').collect();
    if parts.first() != Some(&"conversation") || parts.len() < 3 {
        return source.to_string();
    }
    let channel = parts[1];
    let label = CHANNEL_LABELS.iter().find(|(k, _)| *k == channel).map(|(_, v)| *v).unwrap_or(channel);
    let prefix = format!("{channel}:");
    let Some(after) = source.split_once(&prefix).map(|(_, rest)| rest) else {
        return label.to_string();
    };
    // Locate the ISO timestamp by shape (session ids carry their own
    // colons), same rationale the web original documents.
    let Some(start) = find_iso_timestamp_start(after) else { return label.to_string() };
    let iso = &after[start..];
    let Some((date_part, time_part)) = iso.split_once('T') else { return label.to_string() };
    let date_bits: Vec<&str> = date_part.split('-').collect();
    if date_bits.len() != 3 {
        return label.to_string();
    }
    let month = date_bits[1].trim_start_matches('0');
    let day = date_bits[2].trim_start_matches('0');
    let time_bits: Vec<&str> = time_part.splitn(3, ':').collect();
    if time_bits.len() < 2 {
        return label.to_string();
    }
    format!("{label} · {}/{} {}:{}", if month.is_empty() { "0" } else { month }, if day.is_empty() { "0" } else { day }, time_bits[0], time_bits[1])
}

const DOC_TYPE_FILTERS: [&str; 4] = ["", "sop", "policy", "spec"];

fn filter_label_key(doc_type: &str) -> &'static str {
    match doc_type {
        "sop" => "knowledgeCuration.filter.sop",
        "policy" => "knowledgeCuration.filter.policy",
        "spec" => "knowledgeCuration.filter.spec",
        _ => "knowledgeCuration.filter.all",
    }
}

fn doc_type_badge_variant(doc_type: &str) -> BadgeVariant {
    match doc_type {
        "sop" => BadgeVariant::Info,
        "policy" => BadgeVariant::Warning,
        "spec" => BadgeVariant::Success,
        _ => BadgeVariant::Secondary,
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct KnowledgeCurationState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub selected_agent: Option<String>,
    pub rows: Loadable<Vec<AutoPageRow>>,
    fetched_for: Option<String>,
    pub filter: &'static str,
    pub expanded_path: Option<String>,
    pub preview: Loadable<String>,
    fetched_preview_for: Option<String>,
}

impl KnowledgeCurationState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            selected_agent: None,
            rows: Loadable::Loading,
            fetched_for: None,
            filter: "",
            expanded_path: None,
            preview: Loadable::Loading,
            fetched_preview_for: None,
        }
    }
}

impl Global for KnowledgeCurationState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<KnowledgeCurationState>() {
        cx.set_global(KnowledgeCurationState::new());
    }
}

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<KnowledgeCurationState>().requested_agents {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<KnowledgeCurationState>().requested_agents = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| match result {
        Ok(v) => {
            let list = agents_data::parse_agents_list(&v);
            if cx.global::<KnowledgeCurationState>().selected_agent.is_none() {
                if let Some(first) = list.first() {
                    cx.global_mut::<KnowledgeCurationState>().selected_agent = Some(first.id.clone());
                }
            }
            cx.global_mut::<KnowledgeCurationState>().agents = Loadable::Ready(list);
        }
        Err(e) => cx.global_mut::<KnowledgeCurationState>().agents = Loadable::Failed(e),
    });
}

fn maybe_fetch_rows(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, fetched_for) = {
        let st = cx.global::<KnowledgeCurationState>();
        (st.selected_agent.clone(), st.fetched_for.clone())
    };
    let Some(agent) = agent else { return };
    if fetched_for.as_ref() == Some(&agent) {
        return;
    }
    cx.global_mut::<KnowledgeCurationState>().fetched_for = Some(agent.clone());
    cx.global_mut::<KnowledgeCurationState>().rows = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "wiki.auto_pages", json!({ "agent_id": agent }), |cx, result| {
        cx.global_mut::<KnowledgeCurationState>().rows = result.map(|v| parse_auto_pages(&v).0).into();
    });
}

fn maybe_fetch_preview(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, path, fetched_for) = {
        let st = cx.global::<KnowledgeCurationState>();
        (st.selected_agent.clone(), st.expanded_path.clone(), st.fetched_preview_for.clone())
    };
    let (Some(agent), Some(path)) = (agent, path) else { return };
    if fetched_for.as_ref() == Some(&path) {
        return;
    }
    cx.global_mut::<KnowledgeCurationState>().fetched_preview_for = Some(path.clone());
    cx.global_mut::<KnowledgeCurationState>().preview = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "wiki.read", json!({ "agent_id": agent, "page_path": path }), |cx, result| {
        cx.global_mut::<KnowledgeCurationState>().preview =
            result.map(|v| v.get("content").and_then(Value::as_str).unwrap_or_default().to_string()).into();
    });
}

// ── Filter chips ───────────────────────────────────────────────────────

fn filter_chip(locale: Locale, doc_type: &'static str, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id: SharedString = format!("kc-filter-{}", if doc_type.is_empty() { "all" } else { doc_type }).into();
    div()
        .id(id)
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(i18n::t(locale, filter_label_key(doc_type)))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<KnowledgeCurationState>().filter = doc_type;
            cx.notify();
        }))
}

// ── Feed card ──────────────────────────────────────────────────────────

fn feed_card(locale: Locale, row: &AutoPageRow, expanded: bool, preview: &Loadable<String>, cx: &mut Context<RootView>) -> Div {
    let path = row.path.clone();
    let title: SharedString = if row.title.is_empty() { row.path.clone().into() } else { row.title.clone().into() };
    let source_line = row.sources.last().map(|s| format_source(s));

    let mut header = div()
        .id(SharedString::from(format!("kc-row-{}", row.path)))
        .flex()
        .items_center()
        .gap_2p5()
        .px_4()
        .py_3()
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.25)))
        .child(badge(SharedString::from(row.doc_type_label.clone()), doc_type_badge_variant(&row.doc_type)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title)),
        );
    header = header.on_click(cx.listener(move |_this, _ev, _window, cx| {
        let st = cx.global_mut::<KnowledgeCurationState>();
        st.expanded_path = if st.expanded_path.as_deref() == Some(path.as_str()) { None } else { Some(path.clone()) };
        cx.notify();
    }));

    let meta_line = i18n::tn(
        locale,
        "knowledgeCuration.meta",
        &[
            ("updated", &row.updated),
            ("revisions", &row.revision_count.to_string()),
            ("source", source_line.as_deref().unwrap_or("—")),
        ],
    );

    let mut card = div().flex().flex_col().border_b_1().border_color(theme::border()).child(header).child(
        div().px_4().pb_2p5().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta_line),
    );

    if expanded {
        let body: Div = match preview {
            Loadable::Loading => div().px_4().pb_3().child(skeleton(px(500.), px(48.))),
            Loadable::Failed(e) => div().px_4().pb_3().child(error_row(locale, e)),
            Loadable::Ready(text) => div()
                .px_4()
                .pb_3()
                .max_h(px(160.))
                .overflow_hidden()
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(text.clone())),
        };
        let actions = div()
            .px_4()
            .pb_3p5()
            .flex()
            .gap_2()
            .child(button("kc-approve", i18n::t(locale, "knowledgeCuration.approve"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {}))
            .child(button("kc-reject", i18n::t(locale, "knowledgeCuration.reject"), ButtonVariant::Secondary, true, None, |_ev, _window, _cx| {}));
        card = card.child(body).child(actions);
    }

    card
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_agents(state, cx);
    maybe_fetch_rows(state, cx);
    maybe_fetch_preview(state, cx);

    let locale = state.locale;
    let (rows, filter, expanded_path, preview) = {
        let st = cx.global::<KnowledgeCurationState>();
        (st.rows.clone(), st.filter, st.expanded_path.clone(), st.preview.clone())
    };

    let mut chip_row = div().flex().flex_wrap().gap_1p5();
    for dt in DOC_TYPE_FILTERS {
        chip_row = chip_row.child(filter_chip(locale, dt, dt == filter, cx));
    }

    let body: Div = match &rows {
        Loadable::Loading => div().flex().flex_col().gap_2().p_3().child(skeleton(px(600.), px(60.))).child(skeleton(px(600.), px(60.))),
        Loadable::Failed(e) => div().p_3().child(error_row(locale, e)),
        Loadable::Ready(list) => {
            let filtered: Vec<&AutoPageRow> = list.iter().filter(|r| filter.is_empty() || r.doc_type == filter).collect();
            if filtered.is_empty() {
                div().p_3().child(empty_state("🗂️", i18n::t(locale, "knowledgeCuration.empty"), None, None::<Div>))
            } else {
                let mut feed = div()
                    .max_w(px(640.))
                    .mx_auto()
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded(px(theme::RADIUS_XL))
                    .overflow_hidden()
                    .bg(theme::alpha(theme::SURFACE, 1.0))
                    .border_1()
                    .border_color(theme::surface_border());
                for row in filtered {
                    let is_expanded = expanded_path.as_deref() == Some(row.path.as_str());
                    feed = feed.child(feed_card(locale, row, is_expanded, &preview, cx));
                }
                div().p_3().child(feed)
            }
        }
    };

    div()
        .id("knowledgecuration-page")
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .overflow_y_scroll()
        .child(kc::shell_tabs(locale, "curate", cx))
        .child(chip_row)
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auto_pages_reads_real_handler_shape() {
        let v = json!({ "pages": [
            { "path": "auto/sop/xyz.md", "title": "週三供應商合作備忘", "updated": "2026-08-20T09:00:00Z",
              "doc_type": "sop", "doc_type_label": "流程", "sources": ["conversation:slack:C1:2026-08-04T10:12:33Z"],
              "revision_count": 2, "trust": 0.3 },
        ], "exists": true });
        let (rows, exists) = parse_auto_pages(&v);
        assert!(exists);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].doc_type, "sop");
        assert_eq!(rows[0].doc_type_label, "流程");
        assert_eq!(rows[0].revision_count, 2);
    }

    #[test]
    fn parse_auto_pages_missing_fields_is_empty_not_panicking() {
        assert!(parse_auto_pages(&json!({})).0.is_empty());
        assert!(parse_auto_pages(&json!(null)).0.is_empty());
    }

    #[test]
    fn format_source_conversation_shape() {
        assert_eq!(format_source("conversation:telegram:12345:2026-08-04T10:12:33Z"), "Telegram · 8/4 10:12");
        assert_eq!(format_source("conversation:slack:C1:2026-01-05T03:04:00Z"), "Slack · 1/5 03:04");
    }

    #[test]
    fn format_source_unfamiliar_shape_passes_through() {
        assert_eq!(format_source("manual-entry"), "manual-entry");
        assert_eq!(format_source("conversation:webchat"), "conversation:webchat");
    }

    #[test]
    fn doc_type_filters_cover_the_three_canvas_chips_plus_all() {
        assert_eq!(DOC_TYPE_FILTERS, ["", "sop", "policy", "spec"]);
    }
}
