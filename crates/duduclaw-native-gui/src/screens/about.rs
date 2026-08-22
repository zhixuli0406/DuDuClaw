// S4b (third wave) — Screen "關於" (p13, `nav.rs` id `about`, already the
// sole occupant of the `areaManage`/`FOOTER_ITEMS` "管理" surface — see
// `nav.rs`'s own header comment §"Two settle-related decisions").
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/About.dc.html` —
// a native "About This App" panel grammar (macOS About panel / GNOME
// `AdwAboutWindow`): small-scale, CENTERED (not full-bleed like every other
// real page in this crate) — mark icon → name → tagline → mono version
// badge → update-status line → a boxed-list (Gateway 位址 / 版本 / 授權给) →
// a link row (官方網站/說明文件/開源授權) → a copyright line. The static S4a
// prototype at `screens/prototypes/p13_about.rs` is the pencil sketch of the
// SAME idea (hardcoded strings, no RPC, 🐾 emoji stand-in for the mark);
// this file is the real, RPC-backed page that prototype was standing in
// for — mirrors the exact "prototype → real page" relationship `console.rs`'s
// own header comment describes for p05.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `system.status` (`handle_system_status` ~L18850, payload built by
//     `system_status_payload` ~L18811) → `{ "version", "uptime_seconds",
//     "agents_count", "channels_connected", "gateway_address",
//     "edition_profile" ("personal"|"enterprise"), "quota_warning",
//     "is_appliance" }`. No role gate — any authenticated caller may call
//     this (dispatch arm is bare `"system.status" => ...`, unlike the
//     `require_admin!()`-gated `system.doctor` right below it).
//   `license.status` (`handle_license_status` ~L7124, dispatch gate
//     `require_manager!()` ~L5657 — "the snapshot intentionally omits the
//     raw signature and customer email, so it is safe to show to anyone who
//     can already see operational metrics") → serialized `LicenseSnapshot`;
//     this page reads only `installed` (bool) and `customer_id`
//     (`Option<String>`, an opaque id — never an email, per that same
//     comment) for the "授權给" row.
//   `system.check_update` (`handle_system_check_update` ~L19627, dispatch
//     gate `require_admin!()` ~L6119) → `{ "available", "current_version",
//     "latest_version", ... }`. Read-only (a GitHub metadata fetch — never
//     downloads or installs anything; `system.apply_update`, the actually
//     mutating half, is NOT called anywhere on this page per this task's
//     "不可啟動/重啟" brief). Admin-gated: a manager/employee-role login
//     gets a `CallError::Rejected` here, handled as an ordinary `Failed`
//     state (see `update_status_line` below) — never a page-blocking error.
//
// ── Why this page renders even while NOT authenticated ────────────────────
// Every other RPC-backed page in this crate (`console.rs`, `inbox.rs`,
// `dashboard.rs`) gates its ENTIRE render behind `WsConnState::Authenticated`
// and shows a full-page "🔌 尚未連線" block otherwise — appropriate for pages
// whose only content IS server data. An About panel is different: the task
// brief is explicit that the three-state honesty (loading/ready/failed)
// must hold even offline, with the version badge falling back to "the
// client's own baked-in version + an offline note" rather than blanking the
// whole page. So `render` below never early-returns on connection state;
// each of the three boxed-list rows (and the update-status line) degrades
// independently instead, same per-source-independence principle
// `console.rs`/`inbox.rs`'s own header comments already establish for THEIR
// two-source merges, applied here to three independent single-value sources.
//
// ── Why state lives behind `gpui::Global`, not a new `RootView` field ─────
// Same reasoning `console.rs`'s header comment already spells out in full
// (no `main.rs` edit available/desired this pass — two parallel agents were
// also touching `screens/mod.rs`/`screens/shell.rs` the same round): this
// page reaches for gpui's own app-wide singleton mechanism instead of a
// `RootView` field, keyed by the `AboutState` type, installed lazily by
// `ensure_state` on first render.
//
// ── Honest stub: no "Build 日期" row ────────────────────────────────────
// The design canvas shows a "Build 2026-08-20" caption next to the version
// badge. Grepped for a source of truth first (this crate's own convention:
// "先驗證再套用"): there is NO build-time date instrumentation anywhere in
// this workspace — no `build.rs` in this crate or `duduclaw-gateway`, no
// `vergen`/`built` dependency in `Cargo.lock`, nothing analogous to
// `updater::current_version()`'s `DUDUCLAW_VERSION` env chain for a date.
// Fabricating one (e.g. hardcoding today's date at write-time) would go
// stale on the very next build and is exactly the kind of invented data
// this project's own conventions forbid. This row is dropped entirely
// rather than shipped as a lie; the version badge (real, RPC-sourced when
// connected) carries the "how current is this build" signal instead.
//
// ── No bundled monospace face ──────────────────────────────────────────
// The canvas sets the version badge / Gateway-address values in "Geist
// Mono". `theme.rs::app_font()`'s doc comment lists exactly two bundled
// families (Inter Variable + Noto Sans TC) — no monospace face is
// `include_bytes!`'d into this crate, and `theme.rs` defines no mono-font
// token to "follow" (this task's own "色值走 theme.rs token" instruction is
// about COLOR tokens; there is no font-family token to parallel it). Rather
// than reference an unbundled system font by name (fragile across
// platforms, no established precedent anywhere else in this crate), the
// badge/address text stays on the app's normal bundled font — a deliberate,
// documented visual simplification, not an oversight.

use std::sync::{Arc, OnceLock};

use gpui::{div, img, prelude::*, px, Context, Div, Global, Image, ImageFormat, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::rpc::CallError;
use crate::theme;
use crate::updater::UpdaterStatus;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Mark asset ────────────────────────────────────────────────────────
//
// Copied verbatim from `appliance/branding/png/mark-256.png` (this crate's
// own brand mark, not third-party — no OFL/license file needed the way
// `assets/fonts/OFL-*.txt` is needed for the bundled type faces). 256px
// source for a crisp render at the 72px display size on a 2x/retina
// backing store (128px source would only cover ~1.8x). `Image::from_bytes`
// needs no `AssetSource`/`with_assets(...)` registration on the `App`
// (unlike `img("some/asset/path")`'s `Resource::Embedded` variant) — same
// "embed the bytes directly, no runtime asset lookup" shape `main.rs`
// already uses for the two bundled fonts via `include_bytes!` +
// `add_fonts`, just for an image element instead of the text system. Kept
// in a `OnceLock` so the PNG is decoded into an `Arc<Image>` once, not once
// per render tick (renders happen constantly, per `main.rs`'s 100ms poll
// loop feeding `cx.notify()`).
const MARK_PNG_BYTES: &[u8] = include_bytes!("../../assets/mark-256.png");

fn mark_image() -> Arc<Image> {
    static MARK: OnceLock<Arc<Image>> = OnceLock::new();
    MARK.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, MARK_PNG_BYTES.to_vec()))).clone()
}

// ── Link targets ──────────────────────────────────────────────────────
//
// Verified, not invented: `OFFICIAL_WEBSITE_URL` matches `web/src/pages/
// LicensePage.tsx`'s own `PRICING_URL` host
// (`https://duduclaw.dudustudio.monster`); `LICENSE_URL` points at the
// actual Apache-2.0 `LICENSE` file this repo ships (see root `README.md`'s
// own License section); `DOCS_URL` points at this repo's public `docs/`
// tree (`docs/README.md` is the public index per this project's own
// documentation-classification convention) — this crate has no separately
// hosted docs site to link to instead.
const OFFICIAL_WEBSITE_URL: &str = "https://duduclaw.dudustudio.monster";
const DOCS_URL: &str = "https://github.com/zhixuli0406/DuDuClaw/tree/main/docs";
const LICENSE_URL: &str = "https://github.com/zhixuli0406/DuDuClaw/blob/main/LICENSE";

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SystemInfo {
    pub version: String,
    pub gateway_address: String,
    /// Raw `"personal"`/`"enterprise"` from `system.status` — mapped to a
    /// localized label at render time by [`edition_label`], same
    /// "unresolved kind → raw string, never a raw i18n key" fallback rule
    /// `console.rs::kind_label`'s own doc comment establishes.
    pub edition_profile: String,
}

#[derive(Clone)]
pub struct LicenseInfo {
    pub installed: bool,
    pub customer_id: Option<String>,
}

#[derive(Clone)]
pub struct UpdateCheck {
    pub available: bool,
    pub latest_version: String,
}

/// App-wide singleton page state — see this module's header comment for why
/// `Global` instead of a `RootView` field. Three independent `Loadable`s
/// (not one page-level three-state switch) so e.g. a manager-gated
/// `license.status` failure never blanks the `system.status`-sourced rows —
/// same per-source-independence principle `dashboard.rs`'s module doc
/// comment establishes for its six cards.
pub struct AboutState {
    requested: bool,
    pub system: Loadable<SystemInfo>,
    pub license: Loadable<LicenseInfo>,
    pub update: Loadable<UpdateCheck>,
}

impl AboutState {
    fn new() -> Self {
        Self {
            requested: false,
            system: Loadable::Loading,
            license: Loadable::Loading,
            update: Loadable::Loading,
        }
    }
}

impl Global for AboutState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<AboutState>() {
        cx.set_global(AboutState::new());
    }
}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_system_info(v: &Value) -> SystemInfo {
    SystemInfo {
        version: v.get("version").and_then(Value::as_str).unwrap_or("").to_string(),
        gateway_address: v.get("gateway_address").and_then(Value::as_str).unwrap_or("").to_string(),
        edition_profile: v.get("edition_profile").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

fn parse_license_info(v: &Value) -> LicenseInfo {
    LicenseInfo {
        installed: v.get("installed").and_then(Value::as_bool).unwrap_or(false),
        customer_id: v
            .get("customer_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn parse_update_check(v: &Value) -> UpdateCheck {
    UpdateCheck {
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        latest_version: v.get("latest_version").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

// ── Fetch orchestration (read-only RPCs only — see header comment) ──────

/// Fired from the top of `render` (this page has no `main.rs` poll-loop
/// hook — see header comment). `requested` only latches once the main `/ws`
/// is actually `Authenticated`, so navigating to this page before login
/// resolves doesn't permanently skip the fetch once it does (mirrors
/// `console.rs::maybe_fetch`'s exact gate ordering).
fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<AboutState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<AboutState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "system.status", json!({}), |cx, result| {
        cx.global_mut::<AboutState>().system = result.map(|v| parse_system_info(&v)).into();
    });
    spawn_call(cx, tx.clone(), "license.status", json!({}), |cx, result| {
        cx.global_mut::<AboutState>().license = result.map(|v| parse_license_info(&v)).into();
    });
    spawn_call(cx, tx, "system.check_update", json!({}), |cx, result| {
        cx.global_mut::<AboutState>().update = result.map(|v| parse_update_check(&v)).into();
    });
}

/// One RPC round trip, mutating `AboutState` (the `Global` slot) through
/// `cx` — same shape `console.rs::spawn_call` already established for the
/// Global-state pattern.
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

// ── Display helpers ────────────────────────────────────────────────────

fn edition_label(locale: Locale, raw: &str) -> SharedString {
    match raw {
        "personal" => i18n::t(locale, "native.about.edition.personal"),
        "enterprise" => i18n::t(locale, "native.about.edition.enterprise"),
        "" => "—".into(),
        other => other.to_string().into(),
    }
}

fn gateway_text(locale: Locale, system: &Loadable<SystemInfo>) -> SharedString {
    match system {
        Loadable::Ready(info) if !info.gateway_address.is_empty() => info.gateway_address.clone().into(),
        Loadable::Loading => "…".into(),
        _ => i18n::t(locale, "native.about.notConnected"),
    }
}

fn edition_text(locale: Locale, system: &Loadable<SystemInfo>) -> SharedString {
    match system {
        Loadable::Ready(info) => edition_label(locale, &info.edition_profile),
        Loadable::Loading => "…".into(),
        Loadable::Failed(_) => "—".into(),
    }
}

/// "授權給" row — the one place this page distinguishes "no license RPC
/// answer yet" from "answered, and honestly there's nothing to license"
/// (open-source tier, `installed == false`) from "answered with a real
/// customer id". Never fabricates a friendly display name (the design
/// canvas's "Louis · 嘟嘟數位" is mockup flavor text) — `customer_id` is
/// shown verbatim, per this task's honesty brief.
fn licensed_to_text(locale: Locale, license: &Loadable<LicenseInfo>) -> SharedString {
    match license {
        Loadable::Ready(info) => match &info.customer_id {
            Some(id) => id.clone().into(),
            None if !info.installed => i18n::t(locale, "native.about.licensedTo.openSource"),
            None => "—".into(),
        },
        Loadable::Loading => "…".into(),
        Loadable::Failed(_) => "—".into(),
    }
}

/// The update-status line — `None` while the main `/ws` has never been
/// authenticated (nothing has been checked yet, so showing anything here
/// would be fabricated), otherwise a `(dot color, text)` pair covering all
/// four states the underlying `Loadable` can actually be in, including the
/// admin-gated permission rejection (see header comment on
/// `system.check_update`) — degrades to a quiet muted note, never an alarm.
fn update_status_line(locale: Locale, authenticated: bool, update: &Loadable<UpdateCheck>) -> Option<(u32, SharedString)> {
    if !authenticated {
        return None;
    }
    Some(match update {
        Loadable::Loading => (theme::MUTED_FOREGROUND, i18n::t(locale, "native.about.update.checking")),
        Loadable::Ready(info) if info.available && !info.latest_version.is_empty() => (
            theme::WARNING,
            i18n::t1(locale, "native.about.update.available", "version", &info.latest_version),
        ),
        Loadable::Ready(_) => (theme::SUCCESS, i18n::t(locale, "native.about.update.upToDate")),
        Loadable::Failed(_) => (theme::MUTED_FOREGROUND, i18n::t(locale, "native.about.update.unknown")),
    })
}

// ── Small layout helpers (private to this page — see `console.rs`'s own
// "duplicated rather than importing" precedent for why: `screens::
// prototypes::common`'s `kv_row`/`boxed_group` are `mod`-private to that
// sibling module, not `pub`, so this page grows its own copies rather than
// widening that module's visibility for one more caller) ────────────────

fn kv_row(label: SharedString, value: SharedString, is_last: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(36.))
        .px_3p5()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(value));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn boxed_group(rows: Vec<Div>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .children(rows)
}

fn link_row(id: &'static str, label: SharedString, url: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::PRIMARY, 1.0))
        .cursor_pointer()
        .hover(|style| style.underline())
        .on_click(move |_ev, _window, cx| cx.open_url(url))
        .child(label)
}

// ── WP-C-M3: native-gui app self-update card ─────────────────────────────
//
// A SEPARATE update surface from the "update-status line" already rendered
// in `header` above (`system.check_update`, an RPC to the GATEWAY sidecar
// process — see this page's `AboutState`/`update_status_line` doc comments)
// — this card checks/installs updates for THIS `.app` bundle itself, via
// `state.app_updater` (`updater::UpdaterManager`, see that module's header
// comment for why it's a synchronous-Mutex-poll manager like `sidecar.rs`,
// not a channel actor like `ws_status.rs`). Deliberately independent of the
// gateway `/ws` connection: `state.app_updater.status()` works exactly the
// same whether or not the gateway is reachable, since the update check goes
// straight to GitHub, not through the local gateway.

/// `(dot color, status text)` for every `UpdaterStatus` variant — mirrors
/// `update_status_line`'s own per-state color mapping just above.
fn app_update_status_text(locale: Locale, status: &UpdaterStatus) -> (u32, SharedString) {
    match status {
        UpdaterStatus::Idle => (theme::MUTED_FOREGROUND, i18n::t(locale, "native.about.appUpdate.status.idle")),
        UpdaterStatus::Checking => {
            (theme::MUTED_FOREGROUND, i18n::t(locale, "native.about.appUpdate.status.checking"))
        }
        UpdaterStatus::UpToDate => (theme::SUCCESS, i18n::t(locale, "native.about.appUpdate.status.upToDate")),
        UpdaterStatus::Available { version, .. } => {
            (theme::WARNING, i18n::t1(locale, "native.about.appUpdate.status.available", "version", version))
        }
        UpdaterStatus::Downloading { attempt, max_attempts } => (
            theme::MUTED_FOREGROUND,
            i18n::tn(
                locale,
                "native.about.appUpdate.status.downloading",
                &[("attempt", &attempt.to_string()), ("max", &max_attempts.to_string())],
            ),
        ),
        UpdaterStatus::Installing => {
            (theme::MUTED_FOREGROUND, i18n::t(locale, "native.about.appUpdate.status.installing"))
        }
        UpdaterStatus::ReadyToRestart { version } => {
            (theme::SUCCESS, i18n::t1(locale, "native.about.appUpdate.status.readyToRestart", "version", version))
        }
        UpdaterStatus::Failed { message } => {
            (theme::DESTRUCTIVE, i18n::t1(locale, "native.about.appUpdate.status.failed", "message", message))
        }
    }
}

/// The action button for the current status — one `mds_gpui::button` call
/// per branch rather than a shared helper with a `button_kind` enum: each
/// branch's label AND click behavior differ together, so a shared helper
/// would just relocate the same `match` one level down.
fn app_update_button(locale: Locale, status: &UpdaterStatus, cx: &mut Context<RootView>) -> Stateful<Div> {
    match status {
        UpdaterStatus::Idle | UpdaterStatus::Failed { .. } => crate::mds_gpui::button(
            "about-app-update-check",
            i18n::t(locale, "native.about.appUpdate.button.check"),
            crate::mds_gpui::ButtonVariant::Primary,
            false,
            None,
            cx.listener(|this, _ev, _window, cx| {
                this.app_updater.check();
                cx.notify();
            }),
        ),
        UpdaterStatus::UpToDate => crate::mds_gpui::button(
            "about-app-update-recheck",
            i18n::t(locale, "native.about.appUpdate.button.recheck"),
            crate::mds_gpui::ButtonVariant::Secondary,
            false,
            None,
            cx.listener(|this, _ev, _window, cx| {
                this.app_updater.check();
                cx.notify();
            }),
        ),
        UpdaterStatus::Available { .. } => crate::mds_gpui::button(
            "about-app-update-install",
            i18n::t(locale, "native.about.appUpdate.button.install"),
            crate::mds_gpui::ButtonVariant::Primary,
            false,
            None,
            cx.listener(|this, _ev, _window, cx| {
                this.app_updater.install();
                cx.notify();
            }),
        ),
        UpdaterStatus::Checking | UpdaterStatus::Downloading { .. } | UpdaterStatus::Installing => {
            crate::mds_gpui::button(
                "about-app-update-working",
                i18n::t(locale, "native.about.appUpdate.button.working"),
                crate::mds_gpui::ButtonVariant::Secondary,
                true,
                None,
                |_ev, _window, _cx| {},
            )
        }
        UpdaterStatus::ReadyToRestart { .. } => crate::mds_gpui::button(
            "about-app-update-restart",
            i18n::t(locale, "native.about.appUpdate.button.restart"),
            crate::mds_gpui::ButtonVariant::Primary,
            false,
            None,
            |_ev, _window, cx| {
                // No `RootView` mutation needed here (unlike the other
                // branches above) — relaunching spawns a detached process
                // and quits THIS one, so there is no post-click state left
                // to render. `cx: &mut App` (not `Context<RootView>`) is
                // enough for both `relaunch_current_binary` (a free
                // function, no view access) and `cx.quit()`.
                match crate::updater::relaunch_current_binary() {
                    Ok(()) => cx.quit(),
                    Err(e) => eprintln!("[about] relaunch failed, staying open: {e}"),
                }
            },
        ),
    }
}

fn app_update_card(state: &RootView, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let status = state.app_updater.status();
    let (dot_color, status_text) = app_update_status_text(locale, &status);
    let unsupported = crate::updater::manifest::platform_key().is_none();

    let rows = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.about.appUpdate.currentVersion")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_family("SF Mono")
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(state.app_updater.app_version().to_string()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(status_text)),
        );

    let body: Div = if unsupported {
        // Honest stub for the two placeholder CI legs (Windows/Linux — see
        // `native-gui-desktop-release.yml`'s header comment): no button at
        // all, since there is nothing this build could ever install.
        rows.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                .child(i18n::t(locale, "native.about.appUpdate.platformUnsupported")),
        )
    } else {
        // Release notes, when an update is actually available — the ONE
        // piece of `UpdateManifest::notes` this page shows (never shown for
        // any other status: `Idle`/`Checking`/`UpToDate`/... have nothing
        // meaningful to caption).
        let notes_row = match &status {
            UpdaterStatus::Available { notes, .. } if !notes.trim().is_empty() => Some(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.85))
                    .child(notes.clone()),
            ),
            _ => None,
        };
        rows.children(notes_row).child(div().mt_1().child(app_update_button(locale, &status, cx)))
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.about.appUpdate.title")),
        )
        .child(body)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let authenticated = state.ws_state == WsConnState::Authenticated;
    // Clone the three fields out of the `Global` slot immediately — the
    // borrow ends right here, so the rest of `render` is free to build
    // elements (including `link_row`'s `.on_click(...)`) without holding
    // `cx` borrowed by a live reference into `AboutState` at the same time
    // (the exact hazard `console.rs`'s `ConsoleSnapshot` doc comment
    // describes; this page's data is small/plain enough not to need a
    // dedicated snapshot struct for it).
    let (system, license, update) = {
        let s = cx.global::<AboutState>();
        (s.system.clone(), s.license.clone(), s.update.clone())
    };

    // Version badge: server-reported version when `system.status` has
    // answered, else this BINARY's own baked-in version (`CARGO_PKG_
    // VERSION`, matching `updater::current_version()`'s own final fallback
    // tier) with an explicit offline note — task brief's "版本可先顯示
    // client 端內建版號＋「未連線」註記" literally.
    let (version_text, show_offline_note): (SharedString, bool) = match &system {
        Loadable::Ready(info) if !info.version.is_empty() => (info.version.clone().into(), false),
        _ => (env!("CARGO_PKG_VERSION").into(), true),
    };

    let update_line = update_status_line(locale, authenticated, &update);

    let header = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1p5()
        .child(
            img(mark_image())
                .size(px(72.))
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .shadow(theme::surface_shadow()),
        )
        .child(
            div()
                .mt_2()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child("DuDuClaw"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.about.tagline")),
        )
        .child(
            div()
                .mt_2()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .px_2p5()
                        .py_1()
                        .rounded(px(theme::RADIUS_4XL))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        // WP-NG-debt: the version string reads as a version
                        // string (tabular digits, no ambiguity between "l"/
                        // "1"/"I") when set in the same system monospace
                        // face `chat/markdown.rs::mono_font` already
                        // establishes for this crate's other "this is code/
                        // a version, not prose" text (`SF Mono`, macOS
                        // system font — see that module's doc comment on why
                        // it isn't bundled). Only `.font_family()`, not the
                        // full `Font` struct with fallbacks: a short digits-
                        // and-dots token has none of the glyph-coverage risk
                        // that motivates `mono_font()`'s Menlo/Courier New
                        // fallback chain for arbitrary code content.
                        .font_family("SF Mono")
                        .child(version_text),
                )
                .children(show_offline_note.then(|| {
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                        .child(i18n::t(locale, "native.about.offlineNote"))
                })),
        )
        .children(update_line.map(|(dot_color, text)| {
            div()
                .mt_1()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(7.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(text))
        }));

    let info_group = boxed_group(vec![
        kv_row(i18n::t(locale, "native.about.row.gatewayAddress"), gateway_text(locale, &system), false),
        kv_row(i18n::t(locale, "native.about.row.edition"), edition_text(locale, &system), false),
        kv_row(i18n::t(locale, "native.about.row.licensedTo"), licensed_to_text(locale, &license), true),
    ]);

    let links = div()
        .flex()
        .items_center()
        .gap_3()
        .child(link_row("about-link-website", i18n::t(locale, "native.about.links.website"), OFFICIAL_WEBSITE_URL))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.5)).child("·"))
        .child(link_row("about-link-docs", i18n::t(locale, "native.about.links.docs"), DOCS_URL))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.5)).child("·"))
        .child(link_row("about-link-license", i18n::t(locale, "native.about.links.license"), LICENSE_URL));

    let card = div()
        .w(px(360.))
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(header)
        .child(info_group)
        .child(app_update_card(state, locale, cx))
        .child(links)
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                .child(i18n::t(locale, "native.about.copyright")),
        );

    div().id("about-page").size_full().flex().items_center().justify_center().child(card)
}
