// WP-S5b3-I (2026-08-21) — 記憶 (`Memory.dc.html`, B14). 4-tab switcher over
// a 分類側欄 + 清單 + 詳情 three-column master-detail — mirrors the mockup's
// own left-category/middle-list/right-detail layout exactly (unlike `web/
// src/pages/MemoryPage.tsx`'s dialog-based detail view, see deviation §2
// below).
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `agents.list {}` → `handle_agents_list_filtered` (~L5396); agent picker.
//   `memory.browse {"agent_id","limit"}` (`handle_memory_browse`, ~L13343)
//     → `{ "entries": [...], "signals": [...] }`, each row shaped by
//     `memory_entry_row` (~L3775): "id","agent_id","content","timestamp",
//     "tags","layer","source_event","importance","access_count",
//     "last_accessed","retrievability","stability_days". `signals` (platform
//     learning telemetry, WP15) is NOT read by this page — see deviation §4.
//   `memory.history {"agent_id","memory_id"}` (`handle_memory_history`,
//     ~L13750) → `{ "subject", "predicate", "current_id", "chain": [ {
//     "id","content","valid_from","valid_until","superseded_by",
//     "supersedes","confidence","is_current" } ] }`.
//   Freshness bands mirror the gateway's own `MEMORY_FRESHNESS_BANDS`
//   (~L3829): fresh≥0.70 / stable≥0.40 / fading≥0.15 / else archiving —
//   copied as a literal constant here (see [`FRESHNESS_BANDS`]) so this
//   page's legend can never silently drift from what the server actually
//   used to compute `retrievability`.
//
// ── Canvas fidelity deviations (documented, not silent) ───────────────────
// 1. Four tabs — only 記憶 (this page's own name for the mockup's first tab)
//    is wired to real data. 自主進化/關鍵洞察/知識庫 render an honest "尚未
//    接線" stub instead of a silent blank page — same "only the mockup's
//    shown tab is real, the rest are an honest stub" scope cut `skills.rs`'s
//    own module doc comment already establishes for its "市場" tab (the web
//    `MemoryPage.tsx` actually has 5 tabs — 記憶/wiki/shared/insights/
//    evolution — collapsed to the mockup's simpler 4-tab framing here).
// 2. Detail is an inline third column, not `web/src/components/memory/
//    MemoryBrowser.tsx`'s near-full-screen `Dialog` — the mockup's own
//    layout is a fixed three-column master-detail, matched literally.
// 3. Category rail — the mockup's illustrative "最近對話記得的/長期知道的/做事
//    的方法" (with plausible-looking counts 38/41/15) is the web page's
//    LOCAL 11-category keyword-scored taxonomy (`web/src/lib/memory-
//    category.ts`, client-only heuristic, no backend field). Porting that
//    scorer would add real code for zero new information (the web page's own
//    module doc comment: classification is a pure front-end heuristic).
//    Rebuilt here instead on the entry's real `layer` field (episodic/
//    semantic/procedural, already returned by `memory.browse`) — the three
//    names line up with the mockup's own wording almost exactly (最近對話
//    記得的=episodic, 長期知道的=semantic, 做事的方法=procedural), just backed
//    by a real column instead of a keyword scorer.
// 4. WP15 system-learning signals (`signals` in the RPC response — prediction
//    deviations, mood snapshots) are not surfaced on this page (the web
//    page's own collapsed "平台自身學習" section). Out of this pass's scope;
//    tracked here, not silently dropped.
// 5. Search box — the mockup shows a static icon + "搜尋記憶..." placeholder
//    with no typed-in state visible in its own screenshot; rendered here as
//    the same static decoration (no `memory.search` wiring this pass — the
//    category rail is this page's real filter).
// 6. No forget/delete affordance — the mockup's detail panel has none either
//    (unlike the web dialog's trash icon); not added.
//
// `memory_rows` (category rail + list column) / `memory_detail` (right
// detail column) are nested submodules declared here (`screens/memory/*.rs`)
// — same page-private-sibling shape `plans.rs`/`routines.rs` establish, kept
// under this crate's own <800-line-per-file convention.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::rpc::CallError;
use crate::screens::agents_data::{parse_agents_list, AgentListItem};
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

mod memory_detail;
mod memory_rows;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub timestamp: String,
    pub layer: String,
    pub source_event: String,
    pub access_count: i64,
    pub retrievability: Option<f64>,
}

pub fn parse_memory_entries(v: &Value) -> Vec<MemoryEntry> {
    v.get("entries")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(MemoryEntry {
                        id: e.get("id").and_then(Value::as_str)?.to_string(),
                        content: e.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
                        timestamp: e.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string(),
                        layer: e.get("layer").and_then(Value::as_str).unwrap_or("").to_string(),
                        source_event: e.get("source_event").and_then(Value::as_str).unwrap_or("").to_string(),
                        access_count: e.get("access_count").and_then(Value::as_i64).unwrap_or(0),
                        retrievability: e.get("retrievability").and_then(Value::as_f64),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryChainEntry {
    pub id: String,
    pub content: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub confidence: Option<f64>,
    pub is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryHistory {
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub chain: Vec<MemoryChainEntry>,
}

pub fn parse_memory_history(v: &Value) -> MemoryHistory {
    let chain = v
        .get("chain")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some(MemoryChainEntry {
                        id: c.get("id").and_then(Value::as_str)?.to_string(),
                        content: c.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
                        valid_from: c.get("valid_from").and_then(Value::as_str).map(str::to_string),
                        valid_until: c.get("valid_until").and_then(Value::as_str).map(str::to_string),
                        confidence: c.get("confidence").and_then(Value::as_f64),
                        is_current: c.get("is_current").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    MemoryHistory {
        subject: v.get("subject").and_then(Value::as_str).map(str::to_string),
        predicate: v.get("predicate").and_then(Value::as_str).map(str::to_string),
        chain,
    }
}

// ── Category rail (see this module's header comment §3) ────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCategory {
    All,
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryCategory {
    const ALL: [MemoryCategory; 4] =
        [MemoryCategory::All, MemoryCategory::Episodic, MemoryCategory::Semantic, MemoryCategory::Procedural];

    fn label_key(self) -> &'static str {
        match self {
            MemoryCategory::All => "memoryPage.category.all",
            MemoryCategory::Episodic => "memoryPage.category.episodic",
            MemoryCategory::Semantic => "memoryPage.category.semantic",
            MemoryCategory::Procedural => "memoryPage.category.procedural",
        }
    }

    fn matches(self, layer: &str) -> bool {
        match self {
            MemoryCategory::All => true,
            MemoryCategory::Episodic => layer.eq_ignore_ascii_case("episodic"),
            MemoryCategory::Semantic => layer.eq_ignore_ascii_case("semantic"),
            MemoryCategory::Procedural => layer.eq_ignore_ascii_case("procedural"),
        }
    }
}

/// Which tab (see this module's header comment §1 — only `Memories` is
/// wired to real data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTab {
    Memories,
    Evolution,
    Insights,
    Knowledge,
}

impl MemoryTab {
    const ALL: [MemoryTab; 4] = [MemoryTab::Memories, MemoryTab::Evolution, MemoryTab::Insights, MemoryTab::Knowledge];

    fn label_key(self) -> &'static str {
        match self {
            MemoryTab::Memories => "memoryPage.tab.memories",
            MemoryTab::Evolution => "memoryPage.tab.evolution",
            MemoryTab::Insights => "memoryPage.tab.insights",
            MemoryTab::Knowledge => "memoryPage.tab.knowledge",
        }
    }
}

// ── Freshness (mirrors the gateway's own `MEMORY_FRESHNESS_BANDS`, see this
// module's header comment) ────────────────────────────────────────────────

const FRESHNESS_BANDS: &[(&str, f64)] = &[("fresh", 0.7), ("stable", 0.4), ("fading", 0.15), ("archiving", 0.0)];

fn freshness_band(r: Option<f64>) -> Option<&'static str> {
    let r = r?;
    FRESHNESS_BANDS.iter().find(|(_, lower)| r >= *lower).map(|(k, _)| *k)
}

fn freshness_color(band: &str) -> u32 {
    match band {
        "fresh" => theme::SUCCESS,
        "stable" => theme::BRAND,
        "fading" => theme::WARNING,
        _ => theme::MUTED_FOREGROUND, // archiving
    }
}

fn freshness_label_key(band: &str) -> &'static str {
    match band {
        "fresh" => "memoryPage.freshness.fresh",
        "stable" => "memoryPage.freshness.stable",
        "fading" => "memoryPage.freshness.fading",
        _ => "memoryPage.freshness.archiving",
    }
}

/// Plain-language "kind" — mirrors `web/src/lib/memory-format.ts::
/// memoryLayerMessageId`.
fn layer_label_key(layer: &str) -> Option<&'static str> {
    match layer.to_ascii_lowercase().as_str() {
        "episodic" => Some("memoryPage.layer.episodic"),
        "semantic" => Some("memoryPage.layer.semantic"),
        "procedural" => Some("memoryPage.layer.procedural"),
        _ => None,
    }
}

/// Plain-language "how it was recorded" — mirrors `web/src/lib/memory-
/// format.ts::memorySourceMessageId`'s `byEvent` table (the `byTag` fallback
/// pass is not ported: this page has no per-entry `tags` field parsed, and
/// every origin it covers is also reachable via `source_event`).
fn source_label_key(source_event: &str) -> &'static str {
    match source_event {
        "footprint_distill" => "memoryPage.source.footprint",
        "prediction_episodic" => "memoryPage.source.prediction",
        "reflexion_consolidation" => "memoryPage.source.reflexion",
        "persona_suppression_induction" | "user_profile" | "user_profile_consolidation" => "memoryPage.source.profile",
        "decision_capture" | "decision_resolve" => "memoryPage.source.decision",
        "agent_mood" => "memoryPage.source.mood",
        _ => "memoryPage.source.conversation",
    }
}

/// First line (or first ~90 chars) of a memory's content — the row summary.
/// CJK-safe: truncates by `char`, never a raw byte index (coding convention
/// #1).
pub fn summarize(content: &str, max_chars: usize) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or(content).trim();
    let mut out: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        out.push('…');
    }
    out
}

// ── Global state ───────────────────────────────────────────────────────

pub struct MemoryState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub selected_agent: Option<String>,
    pub tab: MemoryTab,
    pub category: MemoryCategory,
    fetched_for: Option<String>,
    pub entries: Loadable<Vec<MemoryEntry>>,
    pub selected_entry: Option<String>,
    history_for: Option<String>,
    pub history: Loadable<MemoryHistory>,
}

impl MemoryState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            selected_agent: None,
            tab: MemoryTab::Memories,
            category: MemoryCategory::All,
            fetched_for: None,
            entries: Loadable::Loading,
            selected_entry: None,
            history_for: None,
            history: Loadable::Loading,
        }
    }
}

impl Global for MemoryState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<MemoryState>() {
        cx.set_global(MemoryState::new());
    }
}

pub fn resolve_effective_agent(explicit: &Option<String>, rows: &[AgentListItem]) -> Option<String> {
    if let Some(id) = explicit {
        if rows.iter().any(|r| &r.id == id) {
            return Some(id.clone());
        }
    }
    rows.first().map(|r| r.id.clone())
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<MemoryState>().requested_agents {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<MemoryState>().requested_agents = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.global_mut::<MemoryState>().agents = result.map(|v| parse_agents_list(&v)).into();
    });
}

fn maybe_fetch_entries(state: &RootView, cx: &mut Context<RootView>, agent_id: &str) {
    if cx.global::<MemoryState>().fetched_for.as_deref() == Some(agent_id) {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    {
        let g = cx.global_mut::<MemoryState>();
        g.fetched_for = Some(agent_id.to_string());
        g.entries = Loadable::Loading;
        g.selected_entry = None;
    }
    let tx = state.session_tx.clone();
    let key = agent_id.to_string();
    spawn_call(cx, tx, "memory.browse", json!({ "agent_id": agent_id, "limit": 200 }), move |cx, result| {
        if cx.global::<MemoryState>().fetched_for.as_deref() != Some(key.as_str()) {
            return;
        }
        cx.global_mut::<MemoryState>().entries = result.map(|v| parse_memory_entries(&v)).into();
    });
}

fn maybe_fetch_history(state: &RootView, cx: &mut Context<RootView>, agent_id: &str, memory_id: &str) {
    if cx.global::<MemoryState>().history_for.as_deref() == Some(memory_id) {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    {
        let g = cx.global_mut::<MemoryState>();
        g.history_for = Some(memory_id.to_string());
        g.history = Loadable::Loading;
    }
    let tx = state.session_tx.clone();
    let key = memory_id.to_string();
    spawn_call(
        cx,
        tx,
        "memory.history",
        json!({ "agent_id": agent_id, "memory_id": memory_id }),
        move |cx, result| {
            if cx.global::<MemoryState>().history_for.as_deref() != Some(key.as_str()) {
                return;
            }
            cx.global_mut::<MemoryState>().history = result.map(|v| parse_memory_history(&v)).into();
        },
    );
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

// ── Header row: tabs + agent chips ─────────────────────────────────────

fn tab_pill(tab: MemoryTab, active: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let row_id: SharedString = format!("memory-tab-{}", tab.label_key()).into();
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
        .child(i18n::t(locale, tab.label_key()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MemoryState>().tab = tab;
            cx.notify();
        }))
}

fn agent_chip(agent: &AgentListItem, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id_for_click = agent.id.clone();
    let row_id: SharedString = format!("memory-agent-{}", agent.id).into();
    div()
        .id(row_id)
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(if selected { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .bg(theme::alpha(theme::MUTED, if selected { 1.0 } else { 0.0 }))
        .text_color(if selected { theme::alpha(theme::FOREGROUND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) })
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(if agent.display_name.is_empty() { agent.id.clone() } else { agent.display_name.clone() })
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MemoryState>().selected_agent = Some(id_for_click.clone());
            cx.notify();
        }))
}

// ── 記憶 tab body ────────────────────────────────────────────────────

fn memories_tab(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let agents_loadable = cx.global::<MemoryState>().agents.clone();

    match &agents_loadable {
        Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "memoryPage.loading"), None, None::<Div>)),
        Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "memoryPage.loadError", "message", e), None, None::<Div>)),
        Loadable::Ready(rows) if rows.is_empty() => div().flex_1().flex().items_center().justify_center().child(empty_state("👥", i18n::t(locale, "memoryPage.noAgents"), None, None::<Div>)),
        Loadable::Ready(rows) => {
            let Some(agent_id) = resolve_effective_agent(&cx.global::<MemoryState>().selected_agent, rows) else {
                return div().flex_1();
            };
            maybe_fetch_entries(state, cx, &agent_id);

            let mut chip_row = div().flex().flex_wrap().gap_1();
            for a in rows {
                chip_row = chip_row.child(agent_chip(a, a.id == agent_id, cx));
            }

            let search_box = div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2p5()
                .py_1p5()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("🔍")
                .child(i18n::t(locale, "memoryPage.search.placeholder"));

            let entries_loadable = cx.global::<MemoryState>().entries.clone();
            let category = cx.global::<MemoryState>().category;
            let selected_entry = cx.global::<MemoryState>().selected_entry.clone();

            let body: Div = match &entries_loadable {
                Loadable::Loading => div().flex_1().flex().items_center().justify_center().child(empty_state("⏳", i18n::t(locale, "memoryPage.loading"), None, None::<Div>)),
                Loadable::Failed(e) => div().flex_1().flex().items_center().justify_center().child(empty_state("⚠️", i18n::t1(locale, "memoryPage.loadError", "message", e), None, None::<Div>)),
                Loadable::Ready(entries) => {
                    if let Some(id) = &selected_entry {
                        maybe_fetch_history(state, cx, &agent_id, id);
                    }
                    let history = cx.global::<MemoryState>().history.clone();
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .gap_3()
                        .child(memory_rows::category_rail(entries, category, locale, cx))
                        .child(memory_rows::list_column(entries, category, &selected_entry, locale, cx))
                        .child(memory_detail::detail_column(entries, &selected_entry, &history, locale))
                }
            };

            div().flex_1().min_h_0().flex().flex_col().gap_3().child(div().flex().items_center().justify_between().gap_2().flex_wrap().child(chip_row).child(search_box)).child(body)
        }
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_agents(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("memory-page")
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

    let tab = cx.global::<MemoryState>().tab;
    let mut tab_row = div().flex().gap_1().p_1().rounded(px(theme::RADIUS_LG)).bg(theme::alpha(theme::MUTED, 0.6));
    for t in MemoryTab::ALL {
        tab_row = tab_row.child(tab_pill(t, t == tab, locale, cx));
    }

    let header = div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(div().size(px(30.)).rounded(px(theme::RADIUS_MD)).flex().items_center().justify_center().bg(theme::alpha(theme::INFO, 0.14)).child("🧠"))
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.title")))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.subtitle"))),
        );

    let body: Div = if tab == MemoryTab::Memories {
        memories_tab(state, cx)
    } else {
        div().flex_1().flex().items_center().justify_center().child(empty_state("🚧", i18n::t(locale, "memoryPage.tabStub"), Some(i18n::t(locale, "memoryPage.tabStub.desc")), None::<Div>))
    };

    div().id("memory-page").size_full().flex().flex_col().gap_3().p_4().child(header).child(tab_row).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_entries_reads_the_known_fields() {
        let v = json!({ "entries": [
            { "id": "m1", "agent_id": "dudu", "content": "line1\nline2", "timestamp": "t", "tags": [], "layer": "semantic", "source_event": "user_profile", "access_count": 3, "retrievability": 0.82 },
            { "content": "no id" },
        ] });
        let rows = parse_memory_entries(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "m1");
        assert_eq!(rows[0].access_count, 3);
        assert_eq!(rows[0].retrievability, Some(0.82));
    }

    #[test]
    fn category_matches_by_layer_case_insensitively() {
        assert!(MemoryCategory::Episodic.matches("Episodic"));
        assert!(!MemoryCategory::Episodic.matches("semantic"));
        assert!(MemoryCategory::All.matches("anything"));
    }

    #[test]
    fn freshness_band_matches_the_gateway_thresholds() {
        assert_eq!(freshness_band(Some(0.95)), Some("fresh"));
        assert_eq!(freshness_band(Some(0.70)), Some("fresh"));
        assert_eq!(freshness_band(Some(0.69)), Some("stable"));
        assert_eq!(freshness_band(Some(0.40)), Some("stable"));
        assert_eq!(freshness_band(Some(0.39)), Some("fading"));
        assert_eq!(freshness_band(Some(0.15)), Some("fading"));
        assert_eq!(freshness_band(Some(0.10)), Some("archiving"));
        assert_eq!(freshness_band(Some(0.0)), Some("archiving"));
        assert_eq!(freshness_band(None), None);
    }

    #[test]
    fn summarize_truncates_cjk_safely_by_char_count() {
        let s = summarize("嘟嘟事務所的月報整理完成了，準備寄給客戶審核", 6);
        assert_eq!(s.chars().count(), 7); // 6 chars + ellipsis
        assert!(s.ends_with('…'));
    }

    #[test]
    fn summarize_keeps_short_content_untouched() {
        assert_eq!(summarize("短句", 90), "短句");
    }

    #[test]
    fn summarize_skips_leading_blank_lines() {
        assert_eq!(summarize("\n\n實際內容", 90), "實際內容");
    }

    #[test]
    fn parse_memory_history_reads_the_chain() {
        let v = json!({
            "subject": "Louis",
            "predicate": "payment_preference",
            "current_id": "h2",
            "chain": [
                { "id": "h1", "content": "習慣用 Stripe", "valid_from": "2026-01-01T00:00:00Z", "valid_until": "2026-05-01T00:00:00Z", "confidence": 0.7, "is_current": false },
                { "id": "h2", "content": "偏好用 PayUni", "valid_from": "2026-05-01T00:00:00Z", "valid_until": null, "confidence": 0.92, "is_current": true },
            ],
        });
        let h = parse_memory_history(&v);
        assert_eq!(h.subject.as_deref(), Some("Louis"));
        assert_eq!(h.chain.len(), 2);
        assert!(h.chain[1].is_current);
        assert!(!h.chain[0].is_current);
    }

    #[test]
    fn resolve_effective_agent_falls_back_to_first_row() {
        let rows = vec![AgentListItem {
            id: "a".into(),
            display_name: "A".into(),
            role: "".into(),
            department: "".into(),
            status: "active".into(),
            icon: None,
        }];
        assert_eq!(resolve_effective_agent(&None, &rows), Some("a".to_string()));
        assert_eq!(resolve_effective_agent(&Some("missing".to_string()), &rows), Some("a".to_string()));
    }
}
