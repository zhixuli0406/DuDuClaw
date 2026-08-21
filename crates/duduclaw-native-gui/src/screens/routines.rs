// WP-S5b2-D (2026-08-21) — 例行工作 (`Routines.dc.html`). B1 "清單+詳情"
// two-column layout (see the design cover sheet's own processing-recipe
// legend: "B1 清單+詳情 — 沿用頁型1 OmniFocus 三欄降規版…兩欄無需常駐 inspector
// 分頁"), reached via `nav.rs` id `routines` (`TASKS_ITEMS`, already wired by
// this same pass — see `nav.rs`'s module doc comment for the area
// reshuffle).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Routines.dc.html`
// — left column: header ("例行工作 · N 項" + assembled "新增例行工作" button)
// + a scrollable list of routine rows (status dot, name, last-run relative
// time, agent · schedule subtitle, dimmed when disabled). Right column: the
// selected routine's full detail (status badge, "測試執行"/"編輯"/"刪除"
// assembled buttons, 任務內容 card, 排程+上次執行 two-up cards, 從模板帶入 card).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `cron.list` (`handle_cron_list`, ~L16895) → `{ "tasks": [ { "id", "name",
//     "agent_id", "cron", "schedule" (alias of cron), "task", "enabled",
//     "created_at", "updated_at", "last_run_at", "last_status"
//     (`"success"|"failure"|"partial"|null`, `cron_scheduler.rs` tests
//     L946/977), "last_error", "run_count", "failure_count",
//     "notify_channel", "notify_chat_id", "notify_thread_id",
//     "cron_timezone", "trigger_kind", "condition_script", "condition_state",
//     "watch_command" } ] }` — the only RPC this page actually calls.
//   `cron.add`/`cron.update`/`cron.pause`/`cron.resume`/`cron.remove`/
//     `cron.run_now`/`cron.templates` (handlers.rs L16928-17401) are the
//     write-side family the canvas's "新增例行工作"/"測試執行"/"編輯"/"刪除"
//     buttons visually stand in for — per this WP's own brief ("建立/編輯
//     決策類組裝不真按，清單/詳情讀路徑真接") every one of them renders as an
//     inert placeholder (`|_ev, _window, _app| {}`, the exact convention
//     `screens/channels.rs`'s header comment already established for this
//     crate's "assembled, not wired" pages) — no param-builder functions are
//     written for them since (unlike `console.rs`'s real decision RPCs)
//     nothing here ever calls out to the network.
//
// ── Why state lives behind `gpui::Global`, not a `RootView` field ────────
// Same reasoning `console.rs`/`goals.rs`/`channels.rs` already document:
// this pass's brief keeps `main.rs` off-limits (a parallel package owns
// wave-scoped boot wiring), so `RoutinesState` is a `Global` singleton
// lazily installed by `ensure_state`, mutated through `Context<RootView>`
// exactly like every other S5b page.

use chrono::Utc;
use gpui::{div, prelude::*, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::rpc::CallError;
use crate::screens::dashboard::Loadable;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

mod routines_detail;
mod routines_rows;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineRow {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub cron: String,
    pub task: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_run_at: Option<String>,
    /// `"success"|"failure"|"partial"` per `cron_scheduler.rs`'s own tests
    /// — any other/unknown value (or `None`, "never run yet") is rendered
    /// via [`routines_rows::status_dot_color`]'s honest fallback, never
    /// assumed to be one of the three known values.
    pub last_status: Option<String>,
    pub last_error: Option<String>,
}

/// `cron.list` response → rows. Skips any entry missing/empty `id` or
/// `name` (defensive against a malformed payload) rather than panicking or
/// producing an unselectable/unlabeled row.
pub fn parse_routines(v: &Value) -> Vec<RoutineRow> {
    v.get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let id = t.get("id").and_then(Value::as_str)?.to_string();
                    let name = t.get("name").and_then(Value::as_str)?.to_string();
                    if id.is_empty() || name.is_empty() {
                        return None;
                    }
                    Some(RoutineRow {
                        id,
                        name,
                        agent_id: t.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        cron: t.get("cron").and_then(Value::as_str).unwrap_or("").to_string(),
                        task: t.get("task").and_then(Value::as_str).unwrap_or("").to_string(),
                        enabled: t.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        created_at: t.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
                        last_run_at: t.get("last_run_at").and_then(Value::as_str).map(str::to_string),
                        last_status: t.get("last_status").and_then(Value::as_str).map(str::to_string),
                        last_error: t.get("last_error").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Best-effort "白話" (plain-language) cron paraphrase for the canvas's
/// three example shapes ("每天 09:00" / "每週一 08:30" / "每月 1 日 10:00") —
/// a deliberately narrow, pure, unit-tested display formatter (never an RPC,
/// never guessed at runtime). Accepts both 5-field (`min hour dom month
/// dow`) and 6-field (`sec min hour dom month dow`, `normalise_cron`'s own
/// output shape per `handle_cron_add`) expressions, ASCII-only so raw byte
/// splitting on whitespace is safe (coding convention #1 only restricts
/// non-ASCII-safe slicing). Anything outside the three recognized shapes
/// (ranges, steps, lists, `?`, multi-field wildcards) falls back to the raw
/// cron string verbatim — an honest "couldn't paraphrase this" rather than a
/// wrong guess.
pub fn describe_cron(locale: Locale, cron: &str) -> String {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let (min, hour, dom, _month, dow) = match fields.len() {
        5 => (fields[0], fields[1], fields[2], fields[3], fields[4]),
        6 => (fields[1], fields[2], fields[3], fields[4], fields[5]),
        _ => return cron.to_string(),
    };
    let (Ok(m), Ok(h)) = (min.parse::<u32>(), hour.parse::<u32>()) else {
        return cron.to_string();
    };
    if m > 59 || h > 23 {
        return cron.to_string();
    }
    let hhmm = format!("{h:02}:{m:02}");
    if dom == "*" && dow == "*" {
        return i18n::t1(locale, "native.routines.cron.daily", "time", &hhmm).to_string();
    }
    if dom == "*" {
        if let Ok(d) = dow.parse::<u32>() {
            if d <= 7 {
                let weekday = i18n::t(locale, weekday_key(d));
                return i18n::tn(
                    locale,
                    "native.routines.cron.weekly",
                    &[("weekday", weekday.as_ref()), ("time", &hhmm)],
                )
                .to_string();
            }
        }
        return cron.to_string();
    }
    if dow == "*" {
        if let Ok(d) = dom.parse::<u32>() {
            if (1..=31).contains(&d) {
                return i18n::tn(
                    locale,
                    "native.routines.cron.monthly",
                    &[("day", &d.to_string()), ("time", &hhmm)],
                )
                .to_string();
            }
        }
    }
    cron.to_string()
}

/// Cron day-of-week `0`/`7` = Sunday, `1..=6` = Monday..Saturday.
fn weekday_key(d: u32) -> &'static str {
    match d % 7 {
        0 => "native.routines.cron.weekday.sun",
        1 => "native.routines.cron.weekday.mon",
        2 => "native.routines.cron.weekday.tue",
        3 => "native.routines.cron.weekday.wed",
        4 => "native.routines.cron.weekday.thu",
        5 => "native.routines.cron.weekday.fri",
        _ => "native.routines.cron.weekday.sat",
    }
}

// ── Global state ───────────────────────────────────────────────────────

pub struct RoutinesState {
    requested: bool,
    pub routines: Loadable<Vec<RoutineRow>>,
    pub selected: Option<String>,
}

impl RoutinesState {
    fn new() -> Self {
        Self { requested: false, routines: Loadable::Loading, selected: None }
    }

    pub fn request_refresh(&mut self) {
        self.requested = false;
        self.routines = Loadable::Loading;
    }
}

impl Global for RoutinesState {}

pub struct RoutinesSnapshot {
    pub routines: Loadable<Vec<RoutineRow>>,
    pub selected: Option<String>,
}

fn snapshot(cx: &Context<RootView>) -> RoutinesSnapshot {
    let s = cx.global::<RoutinesState>();
    RoutinesSnapshot { routines: s.routines.clone(), selected: s.selected.clone() }
}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<RoutinesState>() {
        cx.set_global(RoutinesState::new());
    }
}

/// Which routine the detail pane should show — the explicit click-selection
/// when it still resolves to a live row, else the first row, else `None`
/// (empty list). Mirrors `console.rs::resolve_selection`'s exact shape.
pub fn resolve_selection<'a>(explicit: &Option<String>, rows: &'a [RoutineRow]) -> Option<&'a RoutineRow> {
    if let Some(id) = explicit {
        if let Some(found) = rows.iter().find(|r| &r.id == id) {
            return Some(found);
        }
    }
    rows.first()
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<RoutinesState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<RoutinesState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "cron.list", json!({}), |cx, result| {
        cx.global_mut::<RoutinesState>().routines = result.map(|v| parse_routines(&v)).into();
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

/// Shared by `routines_rows.rs` (row subtitle) and `routines_detail.rs`
/// (last-run card) — "剛剛"/"N 分鐘前"/... via [`crate::screens::goals::
/// relative_time`], "從未執行" for a `None`/empty timestamp.
pub(super) fn last_run_label(locale: Locale, last_run_at: &Option<String>) -> SharedString {
    match last_run_at.as_deref() {
        Some(ts) if !ts.is_empty() => super::goals::relative_time(locale, ts, Utc::now()),
        _ => i18n::t(locale, "native.routines.status.never"),
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("routines-page")
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

    let snap = snapshot(cx);
    let left = routines_rows::list_column(&snap, locale, cx);
    let right = routines_detail::detail_column(&snap, locale);

    div().id("routines-page").size_full().flex().child(left).child(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_routines_skips_malformed_rows() {
        let v = json!({ "tasks": [
            { "id": "r1", "name": "信箱巡邏", "agent_id": "dudu", "cron": "0 9 * * *", "task": "看信", "enabled": true },
            { "id": "", "name": "no id" },
            { "name": "no id field" },
            { "id": "r2" },
        ] });
        let rows = parse_routines(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "r1");
        assert_eq!(rows[0].agent_id, "dudu");
        assert!(rows[0].enabled);
    }

    #[test]
    fn parse_routines_defaults_enabled_true_when_absent() {
        let v = json!({ "tasks": [ { "id": "r1", "name": "x" } ] });
        let rows = parse_routines(&v);
        assert!(rows[0].enabled);
    }

    #[test]
    fn parse_routines_missing_tasks_key_is_empty() {
        assert!(parse_routines(&json!({})).is_empty());
    }

    #[test]
    fn describe_cron_daily() {
        assert_eq!(describe_cron(Locale::ZhTw, "0 9 * * *"), "每天 09:00");
        // 6-field (normalised) form — same result, seconds field ignored.
        assert_eq!(describe_cron(Locale::ZhTw, "0 0 9 * * *"), "每天 09:00");
    }

    #[test]
    fn describe_cron_weekly() {
        assert_eq!(describe_cron(Locale::ZhTw, "30 8 * * 1"), "每週一 08:30");
        assert_eq!(describe_cron(Locale::ZhTw, "0 10 * * 0"), "每週日 10:00");
    }

    #[test]
    fn describe_cron_monthly() {
        assert_eq!(describe_cron(Locale::ZhTw, "0 10 1 * *"), "每月 1 日 10:00");
    }

    #[test]
    fn describe_cron_falls_back_to_raw_for_unrecognized_shapes() {
        // Step/range/list expressions — not one of the 3 recognized shapes.
        assert_eq!(describe_cron(Locale::ZhTw, "*/15 * * * *"), "*/15 * * * *");
        assert_eq!(describe_cron(Locale::ZhTw, "not a cron"), "not a cron");
        assert_eq!(describe_cron(Locale::ZhTw, ""), "");
    }

    #[test]
    fn describe_cron_rejects_out_of_range_fields() {
        assert_eq!(describe_cron(Locale::ZhTw, "99 9 * * *"), "99 9 * * *");
        assert_eq!(describe_cron(Locale::ZhTw, "0 99 * * *"), "0 99 * * *");
    }

    #[test]
    fn resolve_selection_falls_back_to_first_row() {
        let rows = vec![
            RoutineRow {
                id: "a".into(),
                name: "A".into(),
                agent_id: "".into(),
                cron: "".into(),
                task: "".into(),
                enabled: true,
                created_at: "".into(),
                last_run_at: None,
                last_status: None,
                last_error: None,
            },
        ];
        assert_eq!(resolve_selection(&None, &rows).map(|r| r.id.as_str()), Some("a"));
        assert_eq!(resolve_selection(&Some("missing".to_string()), &rows).map(|r| r.id.as_str()), Some("a"));
        assert_eq!(resolve_selection(&Some("a".to_string()), &rows).map(|r| r.id.as_str()), Some("a"));
        assert_eq!(resolve_selection(&None, &[]), None);
    }
}
