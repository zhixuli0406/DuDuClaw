// macOS menu bar — P0-2 of the "殼回訪修正" pass (2026-08-19, see
// `research/native-os-2026-08/desktop-app-conventions.md` §B P0 item 2 +
// §5 "macOS menu bar 是義務": "Excluding commands from the menu bar — even
// infrequently used or advanced commands — risks making them difficult for
// everyone to find").
//
// gpui's own menu-bar API (`App::set_menus`/`Menu`/`MenuItem`/`actions!`)
// is platform-generic at the TYPE level — nothing in `gpui::Menu` stops it
// from being called on Windows/Linux — but this module is deliberately
// `#[cfg(target_os = "macos")]`-gated end to end anyway, because the task
// brief's Linux answer isn't "the same menu bar, unstyled": research doc §5
// says GNOME apps use a header-bar hamburger menu instead, a genuinely
// different UI, not a degraded macOS one. Verified feasible against
// upstream Zed, not a spike: `crates/zed/src/zed/app_menus.rs` in the
// pinned gpui checkout (`~/.cargo/git/checkouts/zed-*/7a7c3e1/crates/zed/
// src/zed/app_menus.rs`) builds Zed's ENTIRE macOS menu bar on exactly this
// `set_menus`/`Menu`/`MenuItem`/`actions!` surface — same API, larger menu.
//
// `install` (the non-macOS no-op below `mod macos`) gives `main.rs` one
// unconditional call site instead of its own `#[cfg]` branch at the call
// site.

#[cfg(target_os = "macos")]
mod macos {
    use gpui::{actions, App, KeyBinding, Menu, MenuItem, WindowHandle};

    use crate::i18n::{self, Locale};
    use crate::RootView;

    actions!(
        duduclaw_native_gui,
        [OpenSettings, ToggleSidebar, ShowAbout, ShowComponentLibrary, MinimizeWindow, ZoomWindow, QuitApp]
    );

    /// Wires up every action + keybinding + the initial menu bar. Called
    /// once from `main.rs`, after `cx.open_window(...)` returns the
    /// `window` handle every action closure below needs — `App::on_action`
    /// itself only hands back `&mut App`, no window (see
    /// `WindowHandle::update`'s own doc comment), which is why every
    /// closure here re-enters through `window.update(...)` exactly the way
    /// `main.rs`'s background-channel poll loop already does to reach
    /// `RootView` state from outside its own `Context`.
    pub fn install(cx: &mut App, window: WindowHandle<RootView>, locale: Locale) {
        cx.on_action(move |_: &OpenSettings, cx| {
            // `manage` is this crate's settings-hub id — see `nav.rs`'s
            // header comment §1 on why there's no separate "Preferences"
            // id to navigate to instead.
            let _ = window.update(cx, |view, _window, cx| {
                view.active_page = "manage";
                cx.notify();
            });
        });
        cx.on_action(move |_: &ToggleSidebar, cx| {
            let _ = window.update(cx, |view, _window, cx| {
                view.sidebar_collapsed = !view.sidebar_collapsed;
                cx.notify();
            });
        });
        cx.on_action(move |_: &ShowAbout, cx| {
            let _ = window.update(cx, |view, _window, cx| {
                view.active_page = "about";
                cx.notify();
            });
        });
        cx.on_action(move |_: &ShowComponentLibrary, cx| {
            let _ = window.update(cx, |view, _window, cx| {
                view.active_page = "componentLibrary";
                cx.notify();
            });
        });
        cx.on_action(move |_: &MinimizeWindow, cx| {
            // `win` (not `window`) — the outer `window: WindowHandle<
            // RootView>` capture and this closure's own `&mut Window`
            // parameter are different types that both happen to be
            // spellable as "window"; distinct names avoid the shadow.
            let _ = window.update(cx, |_view, win, _cx| win.minimize_window());
        });
        cx.on_action(move |_: &ZoomWindow, cx| {
            let _ = window.update(cx, |_view, win, _cx| win.zoom_window());
        });
        cx.on_action(|_: &QuitApp, cx| cx.quit());

        // Cmd-,＝設定 is the one binding the research doc calls out by name
        // (§5: "標準快捷鍵：Cmd-,＝設定"); Cmd-Q is the platform's own
        // baseline expectation that quitting always has a working shortcut
        // (`crates/gpui/examples/image_gallery.rs` binds exactly this pair
        // the same way — gpui does not supply either automatically).
        // `ToggleSidebar`/`ShowAbout`/`ShowComponentLibrary`/`MinimizeWindow`/
        // `ZoomWindow` intentionally get NO keybinding: none of the HIG
        // sources this pass surveyed name a standard shortcut for any of
        // them (GNOME's F9 sidebar toggle, research doc §5, has no macOS
        // counterpart), and a menu item works by clicking regardless of
        // whether it also has a bound key (verified against
        // `crates/gpui/examples/set_menus.rs`'s own "List Mode"/"Grid"
        // toggle items, which ship with zero `bind_keys` calls at all).
        cx.bind_keys([KeyBinding::new("cmd-,", OpenSettings, None), KeyBinding::new("cmd-q", QuitApp, None)]);

        cx.set_menus(build_menus(locale));
        eprintln!("[native_menu] macOS menu bar installed: App / View / Window / Help");
    }

    /// Menu-title order per the research doc §5: "標準選單組 App/File/Edit/
    /// Format/View/Window/Help 順序固定". This crate has no File/Edit/
    /// Format actions to hang a menu off of (no document model, and the
    /// chat composer's copy/paste/undo already goes through the OS's own
    /// native text-input system, not an app-level Edit menu) — so those
    /// three are simply absent rather than populated with dead
    /// placeholders; the menus that DO exist keep their fixed relative
    /// order (App leads, Help trails).
    fn build_menus(locale: Locale) -> Vec<Menu> {
        vec![
            Menu::new(i18n::t(locale, "app.name")).items(vec![
                MenuItem::action(i18n::t(locale, "native.menu.about"), ShowAbout),
                MenuItem::separator(),
                MenuItem::action(i18n::t(locale, "native.menu.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(i18n::t(locale, "native.menu.quit"), QuitApp),
            ]),
            // View ▸ Show/Hide Sidebar — the research doc requires this
            // explicitly ("View 選單須含 Show/Hide Sidebar"). Component
            // Library rides along as the one other page this crate treats
            // as chrome rather than a normal nav destination (S3's dogfood
            // page — footer-pinned in `nav.rs` for the same reason, see
            // that file's header comment).
            Menu::new(i18n::t(locale, "native.menu.view")).items(vec![
                MenuItem::action(i18n::t(locale, "native.menu.toggleSidebar"), ToggleSidebar),
                MenuItem::action(i18n::t(locale, "nav.componentLibrary"), ShowComponentLibrary),
            ]),
            // Window — Minimize/Zoom via gpui's own `Window::
            // minimize_window`/`zoom_window`, the same pair upstream Zed's
            // `app_menus.rs` Window menu carries (its `super::Minimize`/
            // `super::Zoom` actions are Zed-specific wrappers over the same
            // two `Window` methods — nothing more standard exists to add
            // here without inventing app features this crate doesn't have).
            Menu::new(i18n::t(locale, "native.menu.window")).items(vec![
                MenuItem::action(i18n::t(locale, "native.menu.minimize"), MinimizeWindow),
                MenuItem::action(i18n::t(locale, "native.menu.zoom"), ZoomWindow),
            ]),
            // Help — explicit placeholder per the task brief ("Help（佔
            // 位）"): a disabled item rather than an empty menu, so the
            // menu visibly exists (HIG requires the menu bar itself be
            // complete) without claiming there's real help content behind
            // it yet.
            Menu::new(i18n::t(locale, "native.menu.help"))
                .items(vec![MenuItem::action(i18n::t(locale, "native.menu.helpPlaceholder"), gpui::NoAction).disabled(true)]),
        ]
    }
}

#[cfg(target_os = "macos")]
pub use macos::install;

/// Non-macOS: no menu bar this pass — see this module's header comment for
/// why Linux isn't just "the macOS menu bar, unstyled". Same signature as
/// the macOS `install` so `main.rs` calls this unconditionally.
#[cfg(not(target_os = "macos"))]
pub fn install(_cx: &mut gpui::App, _window: gpui::WindowHandle<crate::RootView>, _locale: crate::i18n::Locale) {}
