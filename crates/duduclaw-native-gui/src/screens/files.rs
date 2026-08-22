// WP-S5b2-F (S5b 第二波) — Screen "檔案" (`nav.rs` id `files`, already a
// top-level "知識與記憶" area item since S5b1-A — this pass wires the
// screen; the `shell.rs` match arm that actually routes `active_id ==
// "files"` here is a sibling package's scope per this task's own "側欄/
// nav/shell.rs 不歸你動" boundary).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Files.dc.
// html` (B3 檔案總管) — left "位置" bookmark column (全部檔案/依 AI 員工/
// 共用檔案) + right content (list/icon 檢視切換 + search + 唯讀表格).
// Functional reference: `web/src/pages/FilesPage.tsx` — see `files_data.
// rs`'s own module doc comment for the full RPC-shape citation and the
// three documented "honest deviations from the design canvas".
//
// State/fetch orchestration lives here; the pure row model, parsing,
// formatting, and query-string building are in `files_data.rs` (kept out
// of this file purely for the same <800-line reason `goals`/`goals_data`
// are split — see that pair's own precedent comment).

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::json;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::files_data::{
    apply_task_filter, build_files_query, file_action_url, format_size, is_office_previewable, is_previewable,
    merge_rows, origin_key_suffix, parse_files_response, task_options, FileBookmark, FileRow, FileViewMode,
    TaskFilter,
};
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── State ──────────────────────────────────────────────────────────────

pub struct FilesState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub bookmark: FileBookmark,
    pub view_mode: FileViewMode,
    pub search: Entity<TextField>,
    pub task_filter: TaskFilter,
    /// `"{bookmark.key()}|{search text}"` of the fetch currently in flight
    /// (or the last one that completed) — dedupes re-render-triggered
    /// re-fetches AND, at apply time, discards a resolved fetch that has
    /// since been superseded by a newer bookmark/search change (no numeric
    /// generation counter needed — the key comparison IS the guard).
    last_fetch_key: Option<String>,
    pub rows: Loadable<Vec<FileRow>>,
}

impl FilesState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            bookmark: FileBookmark::All,
            view_mode: FileViewMode::List,
            search: TextField::new(cx, i18n::t(Locale::ZhTw, "files.filter.search"), false, ""),
            task_filter: TaskFilter::All,
            last_fetch_key: None,
            rows: Loadable::Loading,
        }
    }
}

impl Global for FilesState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<FilesState>() {
        let state = FilesState::new(cx);
        cx.set_global(state);
    }
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if !cx.global::<FilesState>().requested_agents {
        cx.global_mut::<FilesState>().requested_agents = true;
        let tx = state.session_tx.clone();
        spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
            cx.global_mut::<FilesState>().agents = result.map(|v| agents_data::parse_agents_list(&v)).into();
        });
    }

    let (bookmark, search_text) = {
        let g = cx.global::<FilesState>();
        (g.bookmark.clone(), g.search.read(cx).content.clone())
    };
    let key = format!("{}|{}", bookmark.key(), search_text.trim());
    if cx.global::<FilesState>().last_fetch_key.as_deref() == Some(key.as_str()) {
        return;
    }
    // "全部檔案" needs the agent list resolved first — `maybe_fetch` runs
    // again on the next render (agents.list's own completion calls
    // `cx.notify()` via `spawn_call`), so this simply waits rather than
    // firing an aggregate over zero agents.
    if matches!(bookmark, FileBookmark::All) && !matches!(cx.global::<FilesState>().agents, Loadable::Ready(_)) {
        return;
    }

    cx.global_mut::<FilesState>().last_fetch_key = Some(key.clone());
    cx.global_mut::<FilesState>().rows = Loadable::Loading;

    let jwt = state.jwt.clone();
    let tx = state.session_tx.clone();
    match bookmark {
        FileBookmark::Agent(id) => {
            let query = build_files_query(Some(&id), &search_text);
            spawn_single_fetch(cx, tx, jwt, key, query, Some(id));
        }
        FileBookmark::Shared => {
            let query = build_files_query(None, &search_text);
            spawn_single_fetch(cx, tx, jwt, key, query, None);
        }
        FileBookmark::All => {
            let agent_ids: Vec<String> = match &cx.global::<FilesState>().agents {
                Loadable::Ready(v) => v.iter().map(|a| a.id.clone()).collect(),
                _ => Vec::new(),
            };
            spawn_aggregate_fetch(cx, tx, jwt, key, agent_ids, search_text);
        }
    }
}

fn spawn_single_fetch(
    cx: &mut Context<RootView>,
    tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    jwt: Option<String>,
    key: String,
    query: String,
    tag: Option<String>,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::rest_get(&tx, query, jwt);
        let outcome = match rx.await {
            Ok(Ok(v)) => Ok(parse_files_response(&v, tag.as_deref())),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            let g = cx.global_mut::<FilesState>();
            if g.last_fetch_key.as_deref() == Some(key.as_str()) {
                g.rows = outcome.into();
            }
            cx.notify();
        });
    })
    .detach();
}

/// The "全部檔案" bookmark — fans out one real `GET /api/files` per visible
/// agent plus the shared bucket, in parallel, and merges. A bucket this
/// caller cannot access (403) or that otherwise fails contributes zero rows
/// silently (logged to stderr) rather than failing the whole aggregate —
/// see `files_data.rs`'s module doc comment, deviation #2.
fn spawn_aggregate_fetch(
    cx: &mut Context<RootView>,
    tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    jwt: Option<String>,
    key: String,
    agent_ids: Vec<String>,
    search_text: String,
) {
    cx.spawn(async move |weak, cx| {
        let mut tags: Vec<Option<String>> = Vec::with_capacity(agent_ids.len() + 1);
        let mut futs = Vec::with_capacity(agent_ids.len() + 1);
        tags.push(None);
        futs.push(ws_status::rest_get(&tx, build_files_query(None, &search_text), jwt.clone()));
        for id in &agent_ids {
            tags.push(Some(id.clone()));
            futs.push(ws_status::rest_get(&tx, build_files_query(Some(id), &search_text), jwt.clone()));
        }
        let results = futures_util::future::join_all(futs).await;
        let mut buckets = Vec::with_capacity(results.len());
        for (tag, res) in tags.into_iter().zip(results) {
            match res {
                Ok(Ok(v)) => buckets.push(parse_files_response(&v, tag.as_deref())),
                Ok(Err(e)) => {
                    eprintln!("[files] bucket {tag:?} failed: {e}");
                    buckets.push(Vec::new());
                }
                Err(_) => {
                    eprintln!("[files] bucket {tag:?}: session manager gone");
                    buckets.push(Vec::new());
                }
            }
        }
        let merged = merge_rows(buckets);
        let _ = weak.update(cx, |_view, cx| {
            let g = cx.global_mut::<FilesState>();
            if g.last_fetch_key.as_deref() == Some(key.as_str()) {
                g.rows = Loadable::Ready(merged);
            }
            cx.notify();
        });
    })
    .detach();
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: serde_json::Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<serde_json::Value, String>) + 'static,
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

// ── Bookmark column ────────────────────────────────────────────────────

fn bookmark_row(id: SharedString, label: SharedString, active: bool, on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded(px(theme::RADIUS_MD))
        .text_size(px(theme::TEXT_SM))
        .cursor_pointer()
        .when(active, |s| s.bg(theme::alpha(theme::BRAND, 0.12)).text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::MEDIUM))
        .when(!active, |s| s.text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4))))
        .child(label)
        .on_click(on_click)
}

fn bookmark_column(locale: Locale, cx: &mut Context<RootView>) -> Div {
    let (bookmark, agents) = {
        let g = cx.global::<FilesState>();
        (g.bookmark.clone(), g.agents.clone())
    };

    let mut col = div().w(px(190.)).flex_shrink_0().flex().flex_col().gap_0p5().pr_3();
    col = col.child(
        div()
            .px_2()
            .pb_1()
            .text_size(px(theme::TEXT_XS))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "files.bookmarks.location")),
    );
    col = col.child(bookmark_row(
        "files-bookmark-all".into(),
        i18n::t(locale, "files.bookmarks.all"),
        matches!(bookmark, FileBookmark::All),
        cx.listener(|_this, _ev, _window, cx| {
            let g = cx.global_mut::<FilesState>();
            g.bookmark = FileBookmark::All;
            g.task_filter = TaskFilter::All;
            cx.notify();
        }),
    ));

    if let Loadable::Ready(rows) = &agents {
        if !rows.is_empty() {
            col = col.child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_size(px(10.5))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                    .child(i18n::t(locale, "files.bookmarks.byAgent")),
            );
            for a in rows {
                let id = a.id.clone();
                let is_active = matches!(&bookmark, FileBookmark::Agent(x) if x == &id);
                let label: SharedString = a.display_name.clone().into();
                let row_id: SharedString = format!("files-bookmark-agent-{}", a.id).into();
                col = col.child(bookmark_row(row_id, label, is_active, cx.listener(move |_this, _ev, _window, cx| {
                    let g = cx.global_mut::<FilesState>();
                    g.bookmark = FileBookmark::Agent(id.clone());
                    g.task_filter = TaskFilter::All;
                    cx.notify();
                })));
            }
        }
    }

    col.child(
        div()
            .px_2()
            .pt_2()
            .pb_1()
            .text_size(px(10.5))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
            .child(""),
    )
    .child(bookmark_row(
        "files-bookmark-shared".into(),
        i18n::t(locale, "files.scope.shared"),
        matches!(bookmark, FileBookmark::Shared),
        cx.listener(|_this, _ev, _window, cx| {
            let g = cx.global_mut::<FilesState>();
            g.bookmark = FileBookmark::Shared;
            g.task_filter = TaskFilter::All;
            cx.notify();
        }),
    ))
}

// ── Header controls ────────────────────────────────────────────────────

fn view_toggle(locale: Locale, mode: FileViewMode, cx: &mut Context<RootView>) -> Div {
    let seg = |id: &'static str, label: SharedString, active: bool, target: FileViewMode| {
        div()
            .id(id)
            .px_2p5()
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(theme::TEXT_XS))
            .cursor_pointer()
            .when(active, |s| s.bg(theme::alpha(theme::MUTED, 0.7)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).font_weight(gpui::FontWeight::MEDIUM))
            .when(!active, |s| s.text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)))
            .child(label)
            .on_click(cx.listener(move |_this, _ev, _window, cx| {
                cx.global_mut::<FilesState>().view_mode = target;
                cx.notify();
            }))
    };
    div()
        .flex()
        .rounded(px(theme::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .child(seg("files-view-list", i18n::t(locale, "files.view.list"), mode == FileViewMode::List, FileViewMode::List))
        .child(seg("files-view-icon", i18n::t(locale, "files.view.icon"), mode == FileViewMode::Icon, FileViewMode::Icon))
}

fn task_filter_row(locale: Locale, rows: &[FileRow], filter: &TaskFilter, cx: &mut Context<RootView>) -> Option<Div> {
    let opts = task_options(rows);
    if opts.is_empty() {
        return None;
    }
    let pill = |id: SharedString, label: SharedString, active: bool, target: TaskFilter| {
        div()
            .id(id)
            .px_2p5()
            .py_1()
            .rounded(px(theme::RADIUS_4XL))
            .text_size(px(theme::TEXT_XS))
            .cursor_pointer()
            .when(active, |s| s.bg(theme::alpha(theme::BRAND, 0.14)).text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::MEDIUM))
            .when(!active, |s| s.bg(theme::alpha(theme::MUTED, 0.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)))
            .child(label)
            .on_click(cx.listener(move |_this, _ev, _window, cx| {
                cx.global_mut::<FilesState>().task_filter = target.clone();
                cx.notify();
            }))
    };
    let mut row = div().flex().flex_wrap().gap_1p5().child(pill(
        "files-task-all".into(),
        i18n::t(locale, "files.filter.task.all"),
        matches!(filter, TaskFilter::All),
        TaskFilter::All,
    ));
    row = row.child(pill(
        "files-task-none".into(),
        i18n::t(locale, "files.filter.task.none"),
        matches!(filter, TaskFilter::NoTask),
        TaskFilter::NoTask,
    ));
    for id in opts {
        let short = id.chars().take(8).collect::<String>();
        let label = i18n::t1(locale, "files.origin.task", "id", &short);
        let active = matches!(filter, TaskFilter::Task(x) if x == &id);
        row = row.child(pill(format!("files-task-{id}").into(), label, active, TaskFilter::Task(id)));
    }
    Some(row)
}

// ── Row rendering ──────────────────────────────────────────────────────

fn origin_cell(locale: Locale, f: &FileRow) -> Div {
    let variant = if f.origin.as_deref().is_some_and(|o| o != "unknown") { BadgeVariant::Secondary } else { BadgeVariant::Outline };
    let mut cell = div().flex().items_center().gap_1p5().flex_wrap().child(badge(
        i18n::t(locale, &format!("files.origin.{}", origin_key_suffix(f.origin.as_deref()))),
        variant,
    ));
    if let Some(task_id) = &f.task_id {
        let short = task_id.chars().take(8).collect::<String>();
        cell = cell.child(
            div()
                .font_family("SF Mono")
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "files.origin.task", "id", &short)),
        );
    }
    cell
}

fn actions_cell(locale: Locale, jwt: Option<&str>, f: &FileRow) -> Div {
    let name = f.name.clone();
    let agent = f.source_agent.clone();
    let can_preview = is_previewable(&f.name) || is_office_previewable(&f.name);
    let mut cell = div().flex().items_center().justify_end().gap_2();
    if can_preview {
        let path = if is_previewable(&f.name) { "/api/files/download" } else { "/api/files/preview" };
        let url = file_action_url(&crate::api::gateway_base_url(), path, agent.as_deref(), &name, jwt);
        cell = cell.child(
            div()
                .id(SharedString::from(format!("files-preview-{name}")))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .cursor_pointer()
                .hover(|s| s.underline())
                .child(i18n::t(locale, "files.preview"))
                .on_click(move |_ev, _window, cx| cx.open_url(&url)),
        );
    }
    let name2 = f.name.clone();
    let url = file_action_url(&crate::api::gateway_base_url(), "/api/files/download", agent.as_deref(), &name2, jwt);
    cell = cell.child(
        div()
            .id(SharedString::from(format!("files-download-{name2}")))
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::BRAND, 1.0))
            .cursor_pointer()
            .hover(|s| s.underline())
            .child(i18n::t(locale, "files.download"))
            .on_click(move |_ev, _window, cx| cx.open_url(&url)),
    );
    cell
}

fn list_row(locale: Locale, jwt: Option<&str>, f: &FileRow, is_last: bool) -> Div {
    let name = f.display_name.clone().unwrap_or_else(|| f.name.clone());
    let mtime = format_epoch_ms(f.mtime);
    let mut row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_3p5()
        .py_2p5()
        .child(div().flex_1().min_w_0().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).overflow_hidden().child(SharedString::from(name)))
        .child(div().w(px(180.)).flex_shrink_0().child(origin_cell(locale, f)))
        .child(div().w(px(70.)).flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_size(f.size)))
        .child(div().w(px(150.)).flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(mtime))
        .child(div().w(px(110.)).flex_shrink_0().child(actions_cell(locale, jwt, f)));
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

fn icon_card(jwt: Option<&str>, f: &FileRow) -> Stateful<Div> {
    let name = f.display_name.clone().unwrap_or_else(|| f.name.clone());
    let url = file_action_url(&crate::api::gateway_base_url(), "/api/files/download", f.source_agent.as_deref(), &f.name, jwt);
    div()
        .id(SharedString::from(format!("files-icon-{}", f.name)))
        .w(px(128.))
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .p_2()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4)))
        .child(div().text_size(px(28.)).child("📄"))
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).text_align(gpui::TextAlign::Center).overflow_hidden().child(SharedString::from(name)))
        .child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format_size(f.size)))
        .on_click(move |_ev, _window, cx| cx.open_url(&url))
}

fn format_epoch_ms(ms: i64) -> String {
    if ms <= 0 {
        return "—".to_string();
    }
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
        None => "—".to_string(),
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let jwt = state.jwt.clone();
    let search_entity = cx.global::<FilesState>().search.clone();
    let (view_mode, task_filter, rows) = {
        let g = cx.global::<FilesState>();
        (g.view_mode, g.task_filter.clone(), g.rows.clone())
    };

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "nav.files")))
        .children(match &rows {
            Loadable::Ready(v) => Some(div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(v.len().to_string())),
            _ => None,
        })
        .child(div().flex_1())
        .child(view_toggle(locale, view_mode, cx))
        .child(div().w(px(200.)).child(search_entity))
        .child(
            // Honest stub: no date-picker widget exists in this crate yet —
            // see `files_data.rs`'s header comment, deviation #3.
            badge(i18n::t(locale, "files.filter.dateFrom"), BadgeVariant::Outline),
        )
        .child(badge(i18n::t(locale, "files.filter.dateTo"), BadgeVariant::Outline));

    let subtitle = div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "files.subtitle"));

    let filtered: Vec<&FileRow> = match &rows {
        Loadable::Ready(v) => apply_task_filter(v, &task_filter),
        _ => Vec::new(),
    };

    let body = match &rows {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(all) if all.is_empty() => {
            div().child(empty_state("📁", i18n::t(locale, "files.empty"), Some(i18n::t(locale, "files.empty.hint")), None::<Div>))
        }
        Loadable::Ready(_) if filtered.is_empty() => {
            div().child(empty_state("📁", i18n::t(locale, "files.empty.filtered"), None, None::<Div>))
        }
        Loadable::Ready(_) => match view_mode {
            FileViewMode::List => {
                let header_row = div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3p5()
                    .py_2()
                    .bg(theme::alpha(theme::MUTED, 0.35))
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(div().flex_1().child(i18n::t(locale, "files.col.name")))
                    .child(div().w(px(180.)).flex_shrink_0().child(i18n::t(locale, "files.col.origin")))
                    .child(div().w(px(70.)).flex_shrink_0().child(i18n::t(locale, "files.col.size")))
                    .child(div().w(px(150.)).flex_shrink_0().child(i18n::t(locale, "files.col.time")))
                    .child(div().w(px(110.)).flex_shrink_0().text_align(gpui::TextAlign::Right).child(i18n::t(locale, "files.col.actions")));
                let n = filtered.len();
                let mut card = div().rounded(px(theme::RADIUS_XL)).overflow_hidden().bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).flex().flex_col().child(header_row);
                for (i, f) in filtered.iter().enumerate() {
                    card = card.child(list_row(locale, jwt.as_deref(), f, i + 1 == n));
                }
                card
            }
            FileViewMode::Icon => {
                let mut grid = div().flex().flex_wrap().gap_2();
                for f in &filtered {
                    grid = grid.child(icon_card(jwt.as_deref(), f));
                }
                grid
            }
        },
    };

    let task_row = if let Loadable::Ready(all) = &rows { task_filter_row(locale, all, &task_filter, cx) } else { None };

    let right = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(subtitle)
        .children(task_row)
        .child(div().id("files-content-scroll").flex_1().min_h_0().overflow_y_scroll().child(body));

    div()
        .id("files-page")
        .size_full()
        .flex()
        .overflow_hidden()
        .child(bookmark_column(locale, cx))
        .child(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_epoch_ms_zero_or_negative_is_dash() {
        assert_eq!(format_epoch_ms(0), "—");
        assert_eq!(format_epoch_ms(-1), "—");
    }

    #[test]
    fn format_epoch_ms_formats_a_real_timestamp() {
        // 2026-01-01T00:00:00Z
        let s = format_epoch_ms(1_767_225_600_000);
        assert!(s.starts_with("2025-") || s.starts_with("2026-"), "unexpected: {s}");
    }
}
