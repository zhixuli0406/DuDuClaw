// WP-S6b3-R (S6b 第三波, 2026-08-22) — "桌寵浮層" (`MascotOverlay.dc.html`,
// B24 透明浮層). No `nav.rs` entry — self-attached in `screens/shell.rs`
// only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=mascotOverlay`, same "D 先掛好分支就
// 直接可達，未掛就自己掛" precedent every prior self-attached S5b/S6b page
// already establishes.
//
// ── Execution-time attribution: read directly, not assumed ──────────────
// `web/src/pages/MascotOverlayPage.tsx`'s own header comment states it
// plainly: "the Tauri desktop-pet mini route... rendered inside a
// transparent, borderless, always-on-top second window (see `src-tauri/
// src/main.rs`)". Every piece of window-chrome behavior it depends on is
// Tauri-IPC/OS-window-manager territory this crate genuinely cannot reach:
//   - `data-tauri-drag-region` (the whole surface IS the window-move handle)
//     — no such attribute/mechanism exists in gpui; this crate's `main.rs`
//     opens exactly one window (`WindowOptions` with `window_bounds` only,
//     grep-verified — no second `cx.open_window(...)` call anywhere), so
//     there is no secondary transparent/borderless/always-on-top window to
//     even attach drag behavior to.
//   - `openPetContextMenu()` → a native OS right-click menu (Tauri command,
//     `src-tauri`-side, resizes the REAL window to 小/標準/大 and can invoke
//     `open_main_window`). `native_menu.rs` in THIS crate only builds the
//     macOS *menu bar* (via `gpui_macos`), not a per-element native context
//     menu — a different mechanism this crate has never used anywhere.
// This is the SAME structural-gap shape `pet_studio.rs`'s own header
// comment documents (a Tauri-only bridge, not a scope cut) — cross-runtime,
// not a missing feature.
//
// ── UNLIKE `pet_studio.rs`, though: this page's BADGE DATA has a real,
// independently-reachable RPC ─────────────────────────────────────────────
// The web page reads its badge count from `useApprovalsStore().pendingCount`
// (`web/src/stores/approvals-store.ts`) — a Zustand store whose `fetchCount`
// sums THREE sources: `api.approvals.list()` (`.count`), `api.budget.
// incidents()` (`.incidents.length`), and `api.tasks.list({status:
// "blocked"})` (`.tasks.length`). This page ports a SIMPLER one-source
// approximation — `approvals.list` alone (`handle_approvals_list`,
// `duduclaw-gateway/src/handlers.rs` L31367, response shape `{"count":
// N, "approvals":[...]}, verified against `dashboard.rs`'s own identical
// `approvals.list` → `.count` parse, same RPC this crate already calls from
// `dashboard.rs`/`console.rs`/`inbox.rs`) — not the full 3-source sum, a
// deliberate, documented scope cut (the budget-incidents/blocked-tasks
// sources add two more round trips for a demo page whose whole point is the
// STATE MACHINE, not a pixel-exact badge count). So unlike petStudio's "one
// page in this batch with NOTHING to call", this page genuinely fetches
// real backend data for its badge — only the OS-level overlay-window
// mechanics are the honest static/illustrative part.
//
// ── What's real vs. illustrative on this page ─────────────────────────────
// REAL: the pending-approvals count (`approvals.list`, fetched once per
// `Authenticated` session, same `maybe_fetch`-latched shape `console.rs`/
// `inbox.rs` already establish) AND the hover-driven wake/doze state machine
// on the left preview box (`.on_hover(cx.listener(...))` flips `awake`,
// exactly mirroring the web page's own `wake`/`dozeSoon` handlers, minus
// its 2-second doze DEBOUNCE timer — ported as an immediate flip rather than
// a cancellable delayed task, a deliberate simplification for a page whose
// point is demonstrating the 3 states exist and react to hover, not
// reproducing millisecond timing). The face state derivation is copied
// verbatim from `MascotOverlayPage.tsx`'s own line: `awake ? (hasPending ?
// 'curious' : 'idle') : 'sleep'`.
// ILLUSTRATIVE (assembled, not wired — same bar `pet_studio.rs`/
// `migrate.rs::apply_view` already establish for a decision/OS-effecting
// action this crate has no bridge for): the right-click menu's 4 rows
// (小/標準/大 window-resize, 開啟主視窗) render as a static list — "開啟主視窗"
// in particular makes no sense to wire here anyway (this IS the main
// window, there is no second window to jump to).
//
// ── Drawing: DuDu's 3 faces via `gpui::canvas` + `paint_quad` only, same
// "quad primitives, no PathBuilder" convention `pet_studio.rs::pet_
// silhouette` already establishes for this crate's illustrative mascot art
// (a `spike_t7.rs` gradient/PathBuilder demo exists but is explicitly a
// throwaway spike, not a production precedent to build on) ────────────────
// Brand-blue (`theme::BRAND`) rather than petStudio's warm pet palette —
// this is specifically DuDu, the product's own official mascot (see this
// project's CLAUDE.md: "the paw print icon reflects a pet-like
// companionship"), not a stand-in for a user's uploaded photo, so a
// distinct, on-brand color reads as intentional rather than an inconsistent
// reuse of petStudio's illustration style.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::rpc::CallError;
pub use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MascotFace {
    Sleep,
    Idle,
    Curious,
}

// ── State ──────────────────────────────────────────────────────────────

pub struct MascotOverlayState {
    requested: bool,
    pending: Loadable<usize>,
    /// Real client-side hover state — see this file's header comment on why
    /// the web page's 2s doze debounce timer is not ported.
    awake: bool,
}

impl Default for MascotOverlayState {
    fn default() -> Self {
        Self { requested: false, pending: Loadable::Loading, awake: false }
    }
}

impl Global for MascotOverlayState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    cx.default_global::<MascotOverlayState>();
    if cx.global::<MascotOverlayState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<MascotOverlayState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "approvals.list", json!({}), |cx, result| {
        cx.global_mut::<MascotOverlayState>().pending =
            result.map(|v| v.get("count").and_then(Value::as_u64).unwrap_or(0) as usize).into();
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

/// Verbatim port of `MascotOverlayPage.tsx`'s own `const face: DuduFace =
/// awake ? (hasPending ? 'curious' : 'idle') : 'sleep'`.
fn derive_face(awake: bool, pending: usize) -> MascotFace {
    if !awake {
        MascotFace::Sleep
    } else if pending > 0 {
        MascotFace::Curious
    } else {
        MascotFace::Idle
    }
}

// ── DuDu face painter — solid quads only, see header comment ────────────

fn dudu_face(state: MascotFace, size: f32) -> Div {
    div().size(px(size)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                window.paint_quad(gpui::quad(
                    bounds,
                    px(size * 0.30),
                    theme::alpha(theme::BRAND, 1.0),
                    px(0.),
                    gpui::transparent_black(),
                    gpui::BorderStyle::default(),
                ));

                let face_w = size * 0.62;
                let face_h = size * 0.48;
                let face_x = (size - face_w) / 2.0;
                let face_y = size * 0.30;
                let face_bounds = gpui::Bounds::new(
                    bounds.origin + gpui::point(px(face_x), px(face_y)),
                    gpui::size(px(face_w), px(face_h)),
                );
                window.paint_quad(gpui::quad(
                    face_bounds,
                    px(face_h * 0.34),
                    theme::alpha(0xffffff, 0.92),
                    px(0.),
                    gpui::transparent_black(),
                    gpui::BorderStyle::default(),
                ));

                let eye_y = face_y + face_h * 0.32;
                for dx in [face_w * 0.26, face_w * 0.68] {
                    match state {
                        MascotFace::Sleep => {
                            let w = face_w * 0.16;
                            let h = (size * 0.02).max(1.5);
                            let eb = gpui::Bounds::new(
                                bounds.origin + gpui::point(px(face_x + dx - w / 2.0), px(eye_y)),
                                gpui::size(px(w), px(h)),
                            );
                            window.paint_quad(gpui::quad(
                                eb,
                                px(h / 2.0),
                                theme::alpha(theme::BRAND, 1.0),
                                px(0.),
                                gpui::transparent_black(),
                                gpui::BorderStyle::default(),
                            ));
                        }
                        MascotFace::Idle | MascotFace::Curious => {
                            let d = (face_w * 0.15).max(3.0);
                            let eb = gpui::Bounds::new(
                                bounds.origin + gpui::point(px(face_x + dx - d / 2.0), px(eye_y)),
                                gpui::size(px(d), px(d)),
                            );
                            window.paint_quad(gpui::quad(
                                eb,
                                px(d / 2.0),
                                theme::alpha(theme::BRAND, 1.0),
                                px(0.),
                                gpui::transparent_black(),
                                gpui::BorderStyle::default(),
                            ));
                            if state == MascotFace::Curious {
                                let bw = d * 1.3;
                                let bh = (size * 0.015).max(1.5);
                                let bb = gpui::Bounds::new(
                                    bounds.origin + gpui::point(px(face_x + dx - bw / 2.0), px(eye_y - d * 1.1)),
                                    gpui::size(px(bw), px(bh)),
                                );
                                window.paint_quad(gpui::quad(
                                    bb,
                                    px(bh / 2.0),
                                    theme::alpha(theme::WARNING, 1.0),
                                    px(0.),
                                    gpui::transparent_black(),
                                    gpui::BorderStyle::default(),
                                ));
                            }
                        }
                    }
                }
            },
        )
        .size_full(),
    )
}

fn face_label_key(state: MascotFace) -> &'static str {
    match state {
        MascotFace::Sleep => "mascotOverlay.state.sleep",
        MascotFace::Idle => "mascotOverlay.state.idle",
        MascotFace::Curious => "mascotOverlay.state.curious",
    }
}

// ── Left preview box — real hover interaction + real pending count ──────

const PREVIEW_SIZE: f32 = 360.0;

fn preview_box(locale: Locale, awake: bool, pending: Loadable<usize>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let pending_n = match &pending {
        Loadable::Ready(n) => Some(*n),
        _ => None,
    };
    let face = derive_face(awake, pending_n.unwrap_or(0));

    let badge = pending_n.filter(|n| *n > 0).map(|n| {
        div()
            .absolute()
            .top(px(-4.))
            .right(px(-4.))
            .size(px(20.))
            .rounded_full()
            .bg(theme::alpha(theme::WARNING, 1.0))
            .border_2()
            .border_color(theme::alpha(0xffffff, 1.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(10.))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(theme::alpha(0xffffff, 1.0))
            .child(n.to_string())
    });

    let status_line: SharedString = match &pending {
        Loadable::Loading => i18n::t(locale, "mascotOverlay.status.loading"),
        Loadable::Failed(_) => i18n::t(locale, "mascotOverlay.status.error"),
        Loadable::Ready(n) => {
            let state_label = i18n::t(locale, face_label_key(face)).to_string();
            i18n::tn(locale, "mascotOverlay.status.line", &[("state", &state_label), ("count", &n.to_string())])
        }
    };

    div()
        .id("mascot-preview")
        .w(px(PREVIEW_SIZE))
        .h(px(PREVIEW_SIZE + 60.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::MUTED, 0.4))
        .border_1()
        .border_dashed()
        .border_color(theme::surface_border())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(18.))
        .on_hover(cx.listener(|_this, is_hovered: &bool, _window, cx| {
            cx.global_mut::<MascotOverlayState>().awake = *is_hovered;
            cx.notify();
        }))
        .child(
            div()
                .relative()
                .size(px(132.))
                .rounded(px(28.))
                .bg(theme::alpha(0xffffff, 0.9))
                .shadow(theme::floating_shadow())
                .flex()
                .items_center()
                .justify_center()
                .child(dudu_face(face, 84.))
                .children(badge),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .bg(theme::alpha(0xffffff, 0.7))
                .rounded(px(theme::RADIUS_4XL))
                .px_3()
                .py_1()
                .child(status_line),
        )
}

// ── Right column — state-machine legend + illustrative context-menu ─────

fn state_tile(locale: Locale, state: MascotFace, label_key: &'static str, desc_key: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .size(px(64.))
                .rounded(px(16.))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .flex()
                .items_center()
                .justify_center()
                .child(dudu_face(state, 40.)),
        )
        .child(div().text_size(px(10.5)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, label_key)))
        .child(
            div()
                .w(px(76.))
                .text_center()
                .text_size(px(9.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, desc_key)),
        )
}

fn menu_row(locale: Locale, label_key: &'static str, desc_key: &'static str, active: bool, divider: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .when(divider, |d| d.border_t_1().border_color(theme::surface_border()))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, label_key)))
                .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, desc_key))),
        );
    if active {
        row.child(div().size(px(6.)).rounded_full().bg(theme::alpha(theme::BRAND, 1.0)))
    } else {
        row.child(div().text_size(px(12.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.6)).child("›"))
    }
}

fn right_column(locale: Locale) -> Div {
    let states = div()
        .flex()
        .gap_3p5()
        .child(state_tile(locale, MascotFace::Sleep, "mascotOverlay.state.sleep", "mascotOverlay.state.sleep.desc"))
        .child(state_tile(locale, MascotFace::Idle, "mascotOverlay.state.idle", "mascotOverlay.state.idle.desc"))
        .child(state_tile(locale, MascotFace::Curious, "mascotOverlay.state.curious", "mascotOverlay.state.curious.desc"));

    let menu = div()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .overflow_hidden()
        .child(
            div()
                .px_3p5()
                .py_2()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "mascotOverlay.menu.title")),
        )
        .child(menu_row(locale, "mascotOverlay.menu.small", "mascotOverlay.menu.small.desc", false, false))
        .child(menu_row(locale, "mascotOverlay.menu.standard", "mascotOverlay.menu.standard.desc", true, true))
        .child(menu_row(locale, "mascotOverlay.menu.large", "mascotOverlay.menu.large.desc", false, true))
        .child(menu_row(locale, "mascotOverlay.menu.openMain", "mascotOverlay.menu.openMain.desc", false, true));

    let note = div()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::INFO, 0.08))
        .p_3()
        .text_size(px(11.))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "mascotOverlay.precedentNote"));

    div()
        .w(px(300.))
        .flex()
        .flex_col()
        .gap_3p5()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "mascotOverlay.stateMachineTitle")))
        .child(states)
        .child(menu)
        .child(note)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    maybe_fetch(state, cx);

    let (awake, pending) = {
        let s = cx.global::<MascotOverlayState>();
        (s.awake, s.pending.clone())
    };

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "mascotOverlay.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "mascotOverlay.subtitle")));

    let body = div().flex().items_start().gap_6().child(preview_box(locale, awake, pending, cx)).child(right_column(locale));

    let boundary_note = div()
        .text_size(px(10.5))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75))
        .child(i18n::t(locale, "mascotOverlay.boundaryNote"));

    div()
        .id("mascot-overlay-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(header)
        .child(body)
        .child(boundary_note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_face_matches_web_ternary_when_asleep() {
        assert_eq!(derive_face(false, 5), MascotFace::Sleep);
        assert_eq!(derive_face(false, 0), MascotFace::Sleep);
    }

    #[test]
    fn derive_face_curious_when_awake_with_pending() {
        assert_eq!(derive_face(true, 1), MascotFace::Curious);
    }

    #[test]
    fn derive_face_idle_when_awake_with_no_pending() {
        assert_eq!(derive_face(true, 0), MascotFace::Idle);
    }
}
