// D4b — 系統設定 (system settings), the DuDuClaw OS shell's settings app.
//
// ── What this is, and why it is a surface rather than a window ──────────
// Until now this shell's only settings surface was ControlCenter (quick
// toggles) plus the one-page `overlay::pointer_settings` overlay, whose own
// header comment says in as many words: "this shell has no settings
// application and inventing one is explicitly out of scope". This module IS
// that application, so that comment is now historical — read it for the
// pointer page's design reasoning, not for the shell's structure.
//
// It renders as a fifth `crate::surface::Overlay` (a large floating panel
// over Home), NOT as a `duduclaw-comp` toplevel window: it is drawn by the
// shell process itself, in the shell's own gpui tree. WM-3's floating-window
// work applies to real Wayland clients; the shell's own surfaces have never
// been among them.
//
// ── Structure ──────────────────────────────────────────────────────────
// One file per page (`*_page.rs`), plus `client.rs` (the admin RPC bridge)
// and `widgets.rs` (the shared card/row/button primitives) — the same "big
// screen, own directory, many small files" convention `home.rs`+`home/` and
// `overlay.rs`+`overlay/` already established in this crate. Every page's
// STATE is plain data with no gpui types, so the state machines are testable
// without a live window (this crate has no headless UI-click harness — the
// same gap `surface.rs`'s own header comment documents).
//
// ── Honesty contract, which is most of the work here ────────────────────
// Every page shows real system state or says plainly that it cannot. There
// is no seeded demo data anywhere in this directory. Concretely, each page
// distinguishes at least these, and renders them differently:
//
//   * not asked yet / asking          — a neutral line, never an empty state
//   * this machine is not an appliance — the `require_appliance!()` refusal;
//                                        nothing is broken, the feature just
//                                        does not apply (the dev-Mac case)
//   * the service is not installed     — e.g. PipeWire, which is genuinely
//                                        not in the image yet (D5)
//   * the call failed                  — one honest line + the last known
//                                        state left on screen, never an
//                                        optimistic repaint
//
// A control whose backend cannot perform the change is rendered DISABLED
// with the reason next to it — never enabled-and-then-failing.
//
// ── Language ───────────────────────────────────────────────────────────
// Hardcoded zh-TW literals, NOT `crate::i18n`. That matches the larger
// precedent in this crate (Home/ControlCenter/Launcher/Notifications/
// lockscreen are all hardcoded; only OOBE is catalogued, because only OOBE
// has a locale for the operator to have picked yet — every non-OOBE call
// site passes a hardcoded `Locale::ZhTw` anyway). Routing ~150 new strings
// through the catalog would triple them across three exhaustive per-locale
// `match` arms while changing nothing an operator sees. Whole-shell i18n
// remains its own later round, and when it happens this directory is one
// mechanical sweep, exactly like every other non-OOBE surface.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::icons;
use crate::palette::ShellPalette;
use crate::ShellView;

pub(crate) mod about_page;
pub(crate) mod client;
pub(crate) mod datetime_page;
pub(crate) mod display_page;
pub(crate) mod network_page;
pub(crate) mod sound_page;
pub(crate) mod update_page;
pub(crate) mod users_page;
mod widgets;

/// Panel geometry. Centred in this crate's fixed 1440×900 dev-mode window,
/// same derivation `overlay::pointer_settings` documents for its own panel
/// (and the same "no resize handling this round" scope boundary).
///
/// 1180×744 at y=72: the widest page (網路, a Wi-Fi list beside a wired card)
/// needs ~960 of content next to the 208px sidebar, and 744 keeps the bottom
/// edge clear of a 900px screen's dock. NOTHING in this directory scrolls —
/// this crate has no scroll container anywhere (verified: zero
/// `overflow_y_scroll` call sites) — so every page is built to FIT, and the
/// lists that could grow without bound (Wi-Fi networks, display modes) carry
/// an explicit cap plus an honest "還有 N 個" line rather than being clipped.
const PANEL_WIDTH: f32 = 1180.;
const PANEL_HEIGHT: f32 = 744.;
const PANEL_TOP: f32 = 72.;
const PANEL_LEFT: f32 = (1440. - PANEL_WIDTH) / 2.;
const SIDEBAR_WIDTH: f32 = 208.;

/// The seven pages M1 ships. A closed enum, not a list of strings: adding a
/// page has to touch the render dispatch, and a typo cannot produce a
/// category that renders nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsCategory {
    #[default]
    Network,
    Display,
    Sound,
    DateTime,
    Users,
    Update,
    About,
}

impl SettingsCategory {
    /// Sidebar order. `Network` first because it is the one page an operator
    /// standing at a freshly-installed duty box actually needs; `About` last
    /// because it is reference material.
    pub(crate) const ALL: [SettingsCategory; 7] = [
        SettingsCategory::Network,
        SettingsCategory::Display,
        SettingsCategory::Sound,
        SettingsCategory::DateTime,
        SettingsCategory::Users,
        SettingsCategory::Update,
        SettingsCategory::About,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsCategory::Network => "網路",
            SettingsCategory::Display => "顯示",
            SettingsCategory::Sound => "聲音",
            SettingsCategory::DateTime => "日期與時間",
            SettingsCategory::Users => "使用者",
            SettingsCategory::Update => "更新",
            SettingsCategory::About => "關於",
        }
    }

    /// A stable ASCII slug — the gpui element id for the sidebar row, and
    /// the value `DUDUCLAW_SHELL_DEBUG_SETTINGS_PAGE` accepts.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            SettingsCategory::Network => "network",
            SettingsCategory::Display => "display",
            SettingsCategory::Sound => "sound",
            SettingsCategory::DateTime => "datetime",
            SettingsCategory::Users => "users",
            SettingsCategory::Update => "update",
            SettingsCategory::About => "about",
        }
    }

    pub(crate) fn from_slug(raw: &str) -> Option<Self> {
        SettingsCategory::ALL.into_iter().find(|c| c.slug() == raw)
    }

    /// This category's sidebar-row element id. A per-variant LITERAL rather
    /// than `("settings-nav", slug())`: gpui's `ElementId` has no
    /// `From<(&str, &str)>` (only `(&str, u32/u64/usize/EntityId)`), and
    /// building a `SharedString` per row per frame to get around that would
    /// allocate seven times a frame for values that are compile-time
    /// constants.
    fn elem_id(self) -> &'static str {
        match self {
            SettingsCategory::Network => "settings-nav-network",
            SettingsCategory::Display => "settings-nav-display",
            SettingsCategory::Sound => "settings-nav-sound",
            SettingsCategory::DateTime => "settings-nav-datetime",
            SettingsCategory::Users => "settings-nav-users",
            SettingsCategory::Update => "settings-nav-update",
            SettingsCategory::About => "settings-nav-about",
        }
    }

    /// Deliberately reuses this crate's EXISTING icon set — no new SVG asset
    /// is added by this work package. Two of the seven are approximations
    /// and are picked on meaning, not on literal shape:
    ///   * 日期與時間 → globe, because the only thing this page actually
    ///     changes is the TIME ZONE (there is no clock artwork in the set).
    ///   * 關於 → document outline, because that page is a spec sheet.
    ///
    /// Authoring two new icons would mean editing the shared `icons.rs`
    /// registry, which a parallel work package is currently changing.
    fn icon(self) -> &'static str {
        match self {
            SettingsCategory::Network => icons::WIFI,
            SettingsCategory::Display => icons::BRIGHTNESS,
            SettingsCategory::Sound => icons::VOLUME,
            SettingsCategory::DateTime => icons::GLOBE,
            SettingsCategory::Users => icons::AVATAR_DEFAULT,
            SettingsCategory::Update => icons::SOFTWARE_UPDATE,
            SettingsCategory::About => icons::DOCUMENT_OUTLINE,
        }
    }
}

/// What a page knows about one backend answer.
///
/// The same four-state shape `overlay::pointer_settings::PointerLoad`
/// establishes and for the same reason: "haven't asked", "asking", "asked
/// and it failed" and "here it is" are four different facts, and collapsing
/// any two of them is exactly how a surface ends up rendering an empty state
/// that means "loading". Generic because seven pages need it; `Failed`
/// carries the error so the page can tell `not_appliance` apart from a real
/// failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Load<T> {
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(client::SettingsRpcError),
}

// Hand-written rather than derived: `#[derive(Default)]` on a generic enum
// adds a `T: Default` bound, and none of the payloads here have (or should
// need) a meaningful default — "not loaded" is not "an empty one". Clippy's
// `derivable_impls` cannot see that bound difference, which is why it is
// silenced here rather than followed.
#[allow(clippy::derivable_impls)]
impl<T> Default for Load<T> {
    fn default() -> Self {
        Load::NotLoaded
    }
}

impl<T> Load<T> {
    pub(crate) fn value(&self) -> Option<&T> {
        match self {
            Load::Loaded(v) => Some(v),
            _ => None,
        }
    }

    /// Whether a first read still needs to be kicked off. Deliberately
    /// EXCLUDES `Failed`: a page that failed shows its failure and offers a
    /// 重新整理 button, rather than silently retrying forever on every
    /// repaint (which is what a render-time auto-retry would become).
    pub(crate) fn needs_load(&self) -> bool {
        matches!(self, Load::NotLoaded)
    }
}

/// Runtime-mutable state for the whole settings app — lives on `ShellView`
/// as `settings_ui`, the same "plain struct on the view, not a gpui entity"
/// shape `audio_ui`/`pointer_ui` already have (see those fields' own doc
/// comments in `main.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SettingsUiState {
    pub(crate) category: SettingsCategory,
    pub(crate) network: network_page::NetworkPageState,
    pub(crate) display: display_page::DisplayPageState,
    pub(crate) sound: sound_page::SoundPageState,
    pub(crate) datetime: datetime_page::DateTimePageState,
    pub(crate) users: users_page::UsersPageState,
    pub(crate) update: update_page::UpdatePageState,
    pub(crate) about: about_page::AboutPageState,
}

/// Env var naming which page the settings app opens on — the headless-smoke
/// boot hook, same shape and same reason as `DUDUCLAW_SHELL_DEBUG_SURFACE`
/// (see `surface::Overlay::from_debug_env`): this crate has no scriptable
/// UI-click automation, so reaching page five of seven for a screenshot
/// otherwise means a human clicking. An unrecognized value is IGNORED (the
/// app opens on 網路 as usual) — never a panic on a typo'd env var.
///
/// `DUDUCLAW_SHELL_DEBUG_SURFACE=settings DUDUCLAW_SHELL_DEBUG_SETTINGS_PAGE=update`
pub(crate) const DEBUG_PAGE_ENV: &str = "DUDUCLAW_SHELL_DEBUG_SETTINGS_PAGE";

impl SettingsUiState {
    /// The state the shell boots with, honoring [`DEBUG_PAGE_ENV`]. Read
    /// live rather than cached so a smoke run can set it per invocation;
    /// `Default` stays env-free so every test builds a predictable state.
    /// Q1 (2026-08-24): reads through `crate::shipping::debug_env`, so a
    /// shipping binary always boots on the default page.
    pub(crate) fn from_env() -> Self {
        let category = crate::shipping::debug_env(DEBUG_PAGE_ENV)
            .as_deref()
            .and_then(SettingsCategory::from_slug)
            .unwrap_or_default();
        Self { category, ..Self::default() }
    }

    pub(crate) fn select(&mut self, category: SettingsCategory) {
        self.category = category;
    }

    /// Called whenever the settings panel closes, so the next open re-reads
    /// every backend rather than showing a snapshot that may be minutes old
    /// (an IP lease can move, an update can land, a timezone can be changed
    /// from the dashboard) — same contract `PointerUiState::reset`
    /// documents.
    ///
    /// The SELECTED CATEGORY is deliberately reset too: reopening 系統設定
    /// lands on 網路 every time, which is what every surveyed OS does and
    /// what makes the entry point predictable. Goes through `from_env` so a
    /// smoke run pinning a page with [`DEBUG_PAGE_ENV`] gets that page on
    /// every open, not just the first.
    pub(crate) fn reset(&mut self) {
        *self = Self::from_env();
    }
}

// ── Background-call bridge ───────────────────────────────────────────────

/// Runs `work` on a background OS thread and applies its result back on
/// gpui's thread. The single bridge every page in this directory uses.
///
/// Extracted (rather than repeated per page the way
/// `overlay::pointer_settings::poll_into` is repeated per call site) because
/// there are ~12 call sites here, all identical apart from the closure and
/// the settle: `client::call` builds a whole tokio runtime and blocks on a
/// socket, so it must never run on the render thread — the same contract
/// `gateway_client`'s own module doc states and `oobe::steps::account`
/// established.
pub(crate) fn spawn_rpc<T, W, A>(cx: &mut Context<ShellView>, work: W, apply: A)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    A: Fn(&mut ShellView, T, &mut Context<ShellView>) + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // A send failure means the view is gone (window closed mid-call) —
        // dropping the result is correct, and must not panic the thread.
        let _ = tx.send(work());
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(value) => {
                let _ = weak.update(cx, |view, cx| apply(view, value, cx));
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            // The worker thread died without sending (a panic inside
            // `work`). Break rather than spin forever; the page stays in
            // `Loading` and its 重新整理 button is the way out — an honest
            // stuck state beats a fabricated result.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
    })
    .detach();
}

// ── Render ───────────────────────────────────────────────────────────────

pub(super) fn render(
    state: &SettingsUiState,
    fields: &crate::oobe::SettingsFields,
    audio_ui: &crate::audio::AudioUiState,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let mut panel = div()
        .id("overlay-settings-panel")
        .absolute()
        .top(px(PANEL_TOP))
        .left(px(PANEL_LEFT))
        .w(px(PANEL_WIDTH))
        .h(px(PANEL_HEIGHT))
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.98))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow())
        .overflow_hidden();
    if palette.is_dark() {
        // Same dark-only panel-root text colour every sibling overlay sets —
        // see `overlay/controlcenter.rs`'s own header comment.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }

    panel.child(header(palette)).child(
        div()
            .flex_1()
            .flex()
            .min_h(px(0.))
            .child(sidebar(state.category, palette, cx))
            .child(content(state, fields, audio_ui, palette, cx)),
    )
}

fn header(palette: ShellPalette) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(20.))
        .py(px(16.))
        .border_b_1()
        .border_color(palette.border())
        .child(
            div()
                .w(px(34.))
                .h(px(34.))
                .flex_none()
                .rounded(px(10.))
                .bg(theme::alpha(palette.secondary, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .child(icons::icon_or_glyph(&[(icons::SETTINGS, palette.brand)], 18., "設")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).child("系統設定"))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child("這台值班機的網路、螢幕、時間與帳號"),
                ),
        )
}

fn sidebar(active: SettingsCategory, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let mut column = div()
        .w(px(SIDEBAR_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(10.))
        .border_r_1()
        .border_color(palette.border())
        .bg(theme::alpha(palette.surface, 1.0));
    for category in SettingsCategory::ALL {
        column = column.child(sidebar_row(category, category == active, palette, cx));
    }
    column
}

fn sidebar_row(category: SettingsCategory, selected: bool, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let listener = cx.listener(move |view, _ev, _window, cx| {
        view.settings_ui.select(category);
        cx.notify();
    });
    let icon_color = if selected { palette.brand } else { palette.icon_inactive() };
    let mut row = div()
        .id(category.elem_id())
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(10.))
        .py(px(9.))
        .rounded(px(9.))
        .child(
            div()
                .w(px(20.))
                .h(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icons::icon_or_glyph(&[(category.icon(), icon_color)], 16., "·")),
        )
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .text_color(theme::alpha(if selected { palette.brand } else { palette.foreground }, 1.0))
                .child(category.label()),
        );
    if selected {
        row = row.bg(theme::alpha(palette.surface_selected, 1.0));
    } else {
        row = row.cursor_pointer().hover(|s| s.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(listener);
    }
    row
}

fn content(
    state: &SettingsUiState,
    fields: &crate::oobe::SettingsFields,
    audio_ui: &crate::audio::AudioUiState,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let body = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(14.)).p(px(20.));
    match state.category {
        SettingsCategory::Network => network_page::render(body, &state.network, fields, palette, cx),
        SettingsCategory::Display => display_page::render(body, &state.display, palette, cx),
        SettingsCategory::Sound => sound_page::render(body, &state.sound, audio_ui, palette, cx),
        SettingsCategory::DateTime => datetime_page::render(body, &state.datetime, fields, palette, cx),
        SettingsCategory::Users => users_page::render(body, &state.users, fields, palette, cx),
        SettingsCategory::Update => update_page::render(body, &state.update, palette, cx),
        SettingsCategory::About => about_page::render(body, &state.about, palette, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_lists_all_seven_pages_exactly_once() {
        assert_eq!(SettingsCategory::ALL.len(), 7);
        let mut slugs: Vec<&str> = SettingsCategory::ALL.iter().map(|c| c.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), 7, "two categories share a slug");
        // Element ids are separate literals, so they get their own check —
        // a duplicate there is a real gpui collision, not a cosmetic one.
        let mut ids: Vec<&str> = SettingsCategory::ALL.iter().map(|c| c.elem_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 7, "two categories share a gpui element id");
    }

    #[test]
    fn every_category_has_a_label_and_an_installed_icon() {
        for category in SettingsCategory::ALL {
            assert!(!category.label().trim().is_empty(), "{:?} has no label", category);
            // A path not in the registry renders as a blank hole, so this
            // asserts the icon is actually embedded rather than merely
            // spelled plausibly.
            assert!(icons::bytes(category.icon()).is_some(), "{:?}'s icon {} is not registered in icons.rs", category, category.icon());
        }
    }

    #[test]
    fn slugs_round_trip_and_an_unknown_slug_is_refused() {
        for category in SettingsCategory::ALL {
            assert_eq!(SettingsCategory::from_slug(category.slug()), Some(category));
        }
        assert_eq!(SettingsCategory::from_slug("bogus"), None);
        assert_eq!(SettingsCategory::from_slug(""), None);
    }

    #[test]
    fn a_fresh_state_opens_on_network_and_has_asked_nothing() {
        let state = SettingsUiState::default();
        assert_eq!(state.category, SettingsCategory::Network);
        assert!(state.about.info.needs_load(), "nothing may be presented before it has been read");
    }

    #[test]
    fn selecting_a_category_moves_the_selection_and_keeps_page_state() {
        let mut state = SettingsUiState::default();
        state.about.info = Load::Loading;
        state.select(SettingsCategory::Update);
        assert_eq!(state.category, SettingsCategory::Update);
        assert_eq!(state.about.info, Load::Loading, "switching tabs must not discard an in-flight read");
    }

    /// `reset` goes through `from_env`, so with the debug var unset it has
    /// to land exactly on `default()`. Guarded by the same env lock the rest
    /// of this crate's env-touching tests use — `set_var`/`remove_var` are
    /// process-global.
    #[test]
    fn closing_the_panel_forgets_everything_including_the_selected_page() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(DEBUG_PAGE_ENV).ok();
        unsafe { std::env::remove_var(DEBUG_PAGE_ENV) };

        let mut state = SettingsUiState::default();
        state.select(SettingsCategory::About);
        state.about.info = Load::Loading;
        state.reset();

        unsafe {
            if let Some(v) = prev {
                std::env::set_var(DEBUG_PAGE_ENV, v);
            }
        }
        assert_eq!(state, SettingsUiState::default());
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The headless-smoke hook: a recognized slug opens that page, and a
    /// typo'd one is ignored rather than panicking or opening nothing.
    #[test]
    fn the_debug_page_env_selects_a_page_and_ignores_a_typo() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(DEBUG_PAGE_ENV).ok();

        // Q1 (2026-08-24): the hook is now behind the compile-time shipping
        // gate, so what "correct" means here depends on the build. Both
        // branches assert something real rather than one of them being
        // `#[cfg]`-skipped — see `crate::shipping`'s header comment.
        unsafe { std::env::set_var(DEBUG_PAGE_ENV, "update") };
        if crate::shipping::debug_affordances_available() {
            assert_eq!(SettingsUiState::from_env().category, SettingsCategory::Update);
        } else {
            assert_eq!(
                SettingsUiState::from_env().category,
                SettingsCategory::Network,
                "a shipping build must ignore the debug page hook entirely"
            );
        }

        unsafe { std::env::set_var(DEBUG_PAGE_ENV, "bogus") };
        assert_eq!(SettingsUiState::from_env().category, SettingsCategory::Network, "a typo must not open a blank app");

        unsafe { std::env::remove_var(DEBUG_PAGE_ENV) };
        assert_eq!(SettingsUiState::from_env(), SettingsUiState::default());

        unsafe {
            match prev {
                Some(v) => std::env::set_var(DEBUG_PAGE_ENV, v),
                None => std::env::remove_var(DEBUG_PAGE_ENV),
            }
        }
    }

    /// `Failed` must NOT re-arm the auto-load, or a page whose backend is
    /// down would re-dial on every single repaint.
    #[test]
    fn only_a_never_asked_load_arms_the_first_read() {
        let not_loaded: Load<u8> = Load::NotLoaded;
        assert!(not_loaded.needs_load());
        assert!(!Load::Loading::<u8>.needs_load());
        assert!(!Load::Loaded(1u8).needs_load());
        assert!(!Load::<u8>::Failed(client::SettingsRpcError::Timeout).needs_load());
    }

    #[test]
    fn load_value_is_only_available_once_it_really_arrived() {
        assert_eq!(Load::Loaded(7u8).value(), Some(&7));
        assert_eq!(Load::<u8>::Loading.value(), None);
        assert_eq!(Load::<u8>::Failed(client::SettingsRpcError::Timeout).value(), None);
    }

    /// The panel has to fit inside this crate's fixed dev window with its
    /// sidebar and content column both intact.
    #[test]
    fn the_panel_is_centred_and_fits_the_window() {
        assert!(PANEL_LEFT > 0. && PANEL_LEFT * 2. + PANEL_WIDTH <= 1440.);
        assert!(PANEL_TOP + PANEL_HEIGHT <= 900., "the panel would run off a 900px screen");
        assert!(PANEL_WIDTH - SIDEBAR_WIDTH >= 900., "the content column is too narrow for the network page");
    }
}
