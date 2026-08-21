// WP-S6b2-M (S6b 第二波, 2026-08-21) — "資料搬家" (`Migrate.dc.html`, B16
// full-screen wizard). No `nav.rs` entry (this task's own "nav.rs/
// manage_advanced.rs 不動" boundary — `manage_advanced.rs`'s existing 資料
// 搬家 row (`OPS_GROUP_ITEMS`, glyph `'I'`) stays inert this round, same
// "D 先掛好分支就直接可達，未掛就自己掛" precedent every prior self-attached
// S5b/S6b page already establishes). Reachable via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=migrate` today.
//
// ── "全屏精靈" = a `shell.rs`-level root swap, NOT an overlay ─────────────
// The canvas draws this as a hidden-sidebar full-bleed modal task flow ("離
// 開精靈" secondary bar, no app sidebar at all). Ported as an early return
// at the top of `screens::shell::render` that skips the normal sidebar/
// content-list/content_shell three-column composition entirely for
// `active_page == "migrate"` — the same "root 換 child" shape `main.rs`'s
// own `Screen::Language`/`Screen::Login`/`Screen::Shell` match already uses
// (a full swap of what's mounted, still inside gpui's normal flow layout),
// deliberately NOT an absolutely-positioned overlay. This crate's own
// design-history record (`commercial/docs/DESIGN-native-gui-gpui-2026-08.md`
// §14 P8f, a sibling `duduclaw-shell` crate incident) is the cited reason:
// an overlay wrapper left in normal flow got laid out a full screen-height
// below the viewport by that crate's own `shell.rs` flex column and was
// invisible/unclickable for three debugging rounds — a root-level child
// swap has no such stacking-context hazard to begin with.
//
// ── Canvas fidelity: outer window-mockup chrome dropped ─────────────────
// `Migrate.dc.html`'s outer 1240×760 "window" div (3 traffic-light dots +
// centered "資料搬家精靈 — DuDuClaw" titlebar) is the DESIGN TOOL's own
// preview-frame convention for presenting a screenshot-shaped canvas, not
// literal page content — the same chrome every other already-ported canvas
// in this crate (`billing.rs`/`license.rs`/...) already strips before
// rendering (grep confirms zero titlebar/traffic-light code in any of
// them). The real gpui window already has its own OS chrome. Only the
// canvas's SECOND bar (離開精靈 exit affordance + 步驟 X/3 label) is real
// in-wizard content and is kept, as this page's own top bar.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `migrate.scan {platform,source?}` (dispatch L6669, `handle_migrate_scan`
//     L31260 → shared `run_migrate_cli(params, apply=false)` L31272,
//     `require_manager!()`) spawns `current_exe migrate-from <platform>
//     --json` (dry-run — the CLI never writes anything without `--apply`;
//     `migrate_platform_allowed` L28615-28617 whitelists `openclaw`/
//     `hermes`/`paperclip`/`claude-code`) and returns the parsed JSON
//     verbatim: `{"platform","source","dry_run":true,"items":[{"kind",
//     "name","status","reason"}],"summary":{"imported","partial","skipped",
//     "conflict"},"verdict":"COMPLETE"|"DEGRADED"|"PARTIAL","notes":[...],
//     "report_path":null}` (shape cross-checked against `web/src/lib/
//     api.ts`'s `MigrateResult`/`MigrateItem`). The web dashboard's own
//     `MigratePlatform` union only ever sends `openclaw`/`hermes`/
//     `paperclip` — the backend allow-list independently ALSO accepts
//     `"claude-code"`, which is this page's whole reason for existing (the
//     canvas is specifically the Claude-Code-source flow the web dashboard
//     never exposes a picker for). `migrate.apply` (dispatch L6673,
//     `handle_migrate_apply` L31266, same shared driver with `apply=true`,
//     300s timeout) additionally passes `--apply`/`--rename` and WRITES the
//     imported data — a real mutation, deliberately NOT wired to a live
//     click this pass (see below).
//
// ── Wired-for-real vs. assembled-not-wired, and why the line falls here ──
// `migrate.scan` IS wired to a real click (`start_scan`, step 1's own
// button) — it is a side-effect-free dry run (the CLI's own `--json` path
// never touches `--apply`), the same "read-shaped, safe to fire for real"
// bar `screens::identity`'s `identity.resolve` test-button already clears
// for this crate (that module's own header comment calls it "the one page
// in this batch with a genuinely live action"; this page adds a second).
// `migrate.apply` WRITES real data (creates agents, imports skills/memory)
// — assembled, not wired, same "disabled `mds_gpui::button`, zero click
// handler" pattern `distributors.rs`/`users.rs` already establish for
// every decision-type action in this crate, PLUS an explicit CLI-path note
// (`migrate_views::apply_view`) per this task's own brief ("執行套用決策類
// 組裝不真按＋註明 CLI 路徑").
//
// ── Deliberate scope cut vs. the canvas's own step-1 content ─────────────
// The canvas only draws step 2 (this task's own "展示重點"); step 1 has no
// canvas reference. Per this task's "版面禁抄 web" rule, `web/src/pages/
// MigratePage.tsx`'s 3-platform picker grid + free-text source `<Input>` is
// a functional cross-reference ONLY, not ported — this page's step 1 is
// deliberately narrower: a single fixed source (`platform: "claude-code"`,
// server-default `source`, matching the canvas's own step-2 secondary bar
// "來源：Claude Code · ~/.claude"), since the canvas draws no OTHER
// platform anywhere. A future pass adding openclaw/hermes/paperclip back
// in is a straightforward `MigrateState.platform` field addition, not
// attempted here.
//
// `migrate_views` (sibling file) holds the three per-step content panels —
// split out for this crate's own <800-line-per-file convention (this
// file's header comment alone, citing every RPC line number, runs long).

mod migrate_views;

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

const CONTENT_MAX_WIDTH: f32 = 720.0;
const RAIL_WIDTH: f32 = 240.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateStep {
    SelectSource,
    Preview,
    Apply,
}

impl MigrateStep {
    fn ordinal(self) -> u8 {
        match self {
            MigrateStep::SelectSource => 1,
            MigrateStep::Preview => 2,
            MigrateStep::Apply => 3,
        }
    }
}

#[derive(Clone)]
pub struct MigrateItemRow {
    pub kind: String,
    pub name: String,
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Default)]
pub struct MigrateSummary {
    pub imported: i64,
    pub partial: i64,
    pub skipped: i64,
    pub conflict: i64,
}

#[derive(Clone)]
pub struct MigrateResultData {
    pub source: String,
    pub items: Vec<MigrateItemRow>,
    pub summary: MigrateSummary,
    pub verdict: String,
}

/// Pure parse of a `migrate.scan`/`migrate.apply` response — defensive
/// `.and_then` chains only, never panics on a missing/malformed field (this
/// crate's established "honest degrade, not a crash" convention).
pub fn parse_migrate_result(v: &Value) -> MigrateResultData {
    let source = v.get("source").and_then(Value::as_str).unwrap_or("").to_string();
    let items = v
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|it| MigrateItemRow {
                    kind: it.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: it.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    status: it.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
                    reason: it.get("reason").and_then(Value::as_str).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    let s = v.get("summary");
    let summary = MigrateSummary {
        imported: s.and_then(|x| x.get("imported")).and_then(Value::as_i64).unwrap_or(0),
        partial: s.and_then(|x| x.get("partial")).and_then(Value::as_i64).unwrap_or(0),
        skipped: s.and_then(|x| x.get("skipped")).and_then(Value::as_i64).unwrap_or(0),
        conflict: s.and_then(|x| x.get("conflict")).and_then(Value::as_i64).unwrap_or(0),
    };
    let verdict = v.get("verdict").and_then(Value::as_str).unwrap_or("").to_string();
    MigrateResultData { source, items, summary, verdict }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct MigrateState {
    pub step: MigrateStep,
    pub scanning: bool,
    pub scan: Option<MigrateResultData>,
    pub error: Option<String>,
}

impl Default for MigrateState {
    fn default() -> Self {
        Self { step: MigrateStep::SelectSource, scanning: false, scan: None, error: None }
    }
}

impl Global for MigrateState {}

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
        CallError::Rejected(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

/// Step 1's own button handler — the one genuinely live RPC on this page
/// (see this file's header comment for why `migrate.scan` clears the
/// "safe to wire" bar `migrate.apply` does not). 60s server-side timeout
/// (`run_migrate_cli`'s own `dur` for `apply=false`); this client's `rpc.rs`
/// call layer has its own independent timeout underneath that.
fn start_scan(view: &mut RootView, cx: &mut Context<RootView>) {
    if view.ws_state != WsConnState::Authenticated {
        cx.global_mut::<MigrateState>().error = Some(i18n::t(view.locale, "migrate.error.notConnected").to_string());
        cx.notify();
        return;
    }
    {
        let st = cx.global_mut::<MigrateState>();
        if st.scanning {
            return;
        }
        st.scanning = true;
        st.error = None;
    }
    cx.notify();
    let tx = view.session_tx.clone();
    spawn_call(cx, tx, "migrate.scan", json!({"platform": "claude-code"}), |cx, result| {
        let st = cx.global_mut::<MigrateState>();
        st.scanning = false;
        match result {
            Ok(v) => {
                st.scan = Some(parse_migrate_result(&v));
                st.step = MigrateStep::Preview;
            }
            Err(e) => st.error = Some(e),
        }
    });
}

fn exit_wizard(view: &mut RootView, cx: &mut Context<RootView>) {
    *cx.global_mut::<MigrateState>() = MigrateState::default();
    view.active_page = "manageAdvanced";
    cx.notify();
}

// ── Top bar (real content — see header comment for what was dropped) ────

fn top_bar(locale: Locale, step: MigrateStep, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("migrate-topbar")
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .px_5()
        .py_3()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .id("migrate-exit")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "migrate.exitWizard"))
                .on_click(cx.listener(|this, _ev, _window, cx| exit_wizard(this, cx))),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                .child(i18n::tn(locale, "migrate.stepOf", &[("step", &step.ordinal().to_string()), ("total", "3")])),
        )
}

// ── Left rail (KMyMoney-style persistent step list) ──────────────────────

#[allow(clippy::too_many_arguments)]
fn rail_item(
    locale: Locale,
    idx: u8,
    title_key: &'static str,
    desc: SharedString,
    current: MigrateStep,
    target: MigrateStep,
    clickable: bool,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let done = target.ordinal() < current.ordinal();
    let active = target == current;

    let marker = if done {
        div()
            .size(px(22.))
            .flex_shrink_0()
            .rounded_full()
            .bg(theme::alpha(theme::SUCCESS, 0.14))
            .text_color(theme::alpha(theme::SUCCESS, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::BOLD)
            .child("✓")
    } else if active {
        div()
            .size(px(22.))
            .flex_shrink_0()
            .rounded_full()
            .bg(theme::alpha(theme::BRAND, 1.0))
            .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::BOLD)
            .child(idx.to_string())
    } else {
        div()
            .size(px(22.))
            .flex_shrink_0()
            .rounded_full()
            .bg(theme::alpha(theme::MUTED, 1.0))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::BOLD)
            .child(idx.to_string())
    };

    let title_color =
        if active { theme::alpha(theme::BRAND, 1.0) } else if done { theme::alpha(theme::FOREGROUND, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 1.0) };

    let mut row = div()
        .id(SharedString::from(format!("migrate-rail-{idx}")))
        .flex()
        .items_start()
        .gap_2p5()
        .px_2p5()
        .py_2()
        .rounded(px(theme::RADIUS_MD))
        .when(active, |s| s.bg(theme::alpha(theme::BRAND, 0.10)))
        .child(marker)
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::MEDIUM })
                        .text_color(title_color)
                        .child(i18n::t(locale, title_key)),
                )
                .child(div().mt_0p5().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.85)).child(desc)),
        );

    if clickable && !active {
        row = row.cursor_pointer().hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))).on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MigrateState>().step = target;
            cx.notify();
        }));
    }

    row
}

fn left_rail(locale: Locale, step: MigrateStep, has_scan: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("migrate-rail")
        .w(px(RAIL_WIDTH))
        .flex_shrink_0()
        .h_full()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::border())
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .p(px(18.))
        .child(div().mb_0p5().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.title")))
        .child(div().mb_5().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.subtitle")))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(rail_item(locale, 1, "migrate.step1.title", i18n::t(locale, "migrate.step1.desc"), step, MigrateStep::SelectSource, true, cx))
                .child(rail_item(locale, 2, "migrate.step2.title", i18n::t(locale, "migrate.step2.desc"), step, MigrateStep::Preview, has_scan, cx))
                .child(rail_item(locale, 3, "migrate.step3.title", SharedString::default(), step, MigrateStep::Apply, has_scan, cx)),
        )
}

// ── Top-level render ──────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Div {
    cx.default_global::<MigrateState>();
    let locale = state.locale;
    // Clone the small snapshot this render pass needs OUT of the global
    // borrow before building any `cx.listener(...)` closures below — a live
    // `cx.global::<MigrateState>()` borrow cannot coexist with the `&mut
    // Context<RootView>` those closures need (same pattern `screens::
    // identity::render`'s own `config.clone()` line already establishes).
    let (step, scanning, error, scan) = {
        let st = cx.global::<MigrateState>();
        (st.step, st.scanning, st.error.clone(), st.scan.clone())
    };

    let rail = left_rail(locale, step, scan.is_some(), cx);
    let content = match step {
        MigrateStep::SelectSource => migrate_views::select_source_view(locale, scanning, error, cx),
        MigrateStep::Preview => migrate_views::preview_view(locale, scan, cx),
        MigrateStep::Apply => migrate_views::apply_view(locale, scan, cx),
    };

    // Plain `Div` (no `.id(...)`), NOT `Stateful<Div>` — `screens::shell::
    // render`'s own signature is `-> Div` and this page's early-return
    // bypass (see that file's own `active_id == "migrate"` branch) hands
    // this value straight back as the function's return value, not through
    // a `.child(...)` call that would accept either type.
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .child(top_bar(locale, step, cx))
        .child(
            div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(rail)
                .child(div().id("migrate-content").flex_1().overflow_y_scroll().p_6().child(div().max_w(px(CONTENT_MAX_WIDTH)).child(content))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_migrate_result_reads_every_field() {
        let v = json!({
            "source": "~/.claude",
            "items": [
                {"kind": "agent", "name": "biz-dev", "status": "imported", "reason": null},
                {"kind": "agent", "name": "coder", "status": "conflict", "reason": "識別碼已被現有員工使用"},
            ],
            "summary": {"imported": 1, "partial": 0, "skipped": 0, "conflict": 1},
            "verdict": "DEGRADED",
            "notes": [],
            "report_path": null,
        });
        let data = parse_migrate_result(&v);
        assert_eq!(data.source, "~/.claude");
        assert_eq!(data.items.len(), 2);
        assert_eq!(data.items[0].status, "imported");
        assert_eq!(data.items[1].reason.as_deref(), Some("識別碼已被現有員工使用"));
        assert_eq!(data.summary.conflict, 1);
        assert_eq!(data.verdict, "DEGRADED");
    }

    #[test]
    fn parse_migrate_result_missing_fields_default_empty_not_panicking() {
        let data = parse_migrate_result(&json!({}));
        assert!(data.items.is_empty());
        assert_eq!(data.summary.imported, 0);
        assert_eq!(data.verdict, "");
    }

    #[test]
    fn migrate_step_ordinal_orders_select_preview_apply() {
        assert!(MigrateStep::SelectSource.ordinal() < MigrateStep::Preview.ordinal());
        assert!(MigrateStep::Preview.ordinal() < MigrateStep::Apply.ordinal());
    }
}
