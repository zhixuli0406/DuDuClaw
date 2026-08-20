// mds_gpui — MDS (Multica-derived Design System) component facade for the
// native GUI. Every component here consumes `theme.rs` tokens exclusively
// (no ad-hoc color/radius/type values) — same discipline `screens/shell.rs`
// and `screens/login.rs` already established before this module existed.
//
// ── D4 spike outcome (2026-08-19): `longbridge/gpui-component` REJECTED as
// a foundation ──────────────────────────────────────────────────────────
// Evaluated wrapping `gpui-component` (a full pre-built gpui design-system
// crate, `crates/ui` + `crates/base` + friends) instead of hand-rolling.
// Rejected on hard evidence, not a guess:
//
//   1. Rev alignment breaks type-compatibility, not just "might need a
//      bump". `gpui-component`'s own `Cargo.toml` pins its `gpui` git
//      dependency with NO `rev` (floats to zed's default-branch HEAD at
//      lock time). This crate pins `gpui` to a fixed `rev` (see main.rs's
//      gotcha list — required for reproducible builds against a pre-1.0,
//      frequently-breaking dependency). Adding `gpui-component` alongside
//      our existing pinned `gpui` was tested for real: a throwaway
//      workspace with both dependencies, `cargo generate-lockfile`,
//      resolved to TWO separate `gpui` package entries —
//      `rev=7a7c3e1d2f03195c5fa19bc890da330ad7f3abef` (ours) and
//      `28c0f4aef89d298a08176977d9839671827f5528` (gpui-component's
//      floating pull — notably not even the `e0931d5a...` rev committed in
//      gpui-component's OWN `Cargo.lock`, since nothing here vendors that
//      lockfile). Two `gpui` instances means two distinct, incompatible
//      `Div`/`Hsla`/`Context`/etc. types — `gpui-component` widgets could
//      not be composed into this crate's existing screens at all. The only
//      fix is dropping our own `rev` pin and following gpui-component's
//      moving target, which reopens exactly the "existing code might not
//      compile" risk this task's brief flagged, as an ongoing burden, not a
//      one-time migration (re-verified every time either repo moves).
//   2. Partial theming coverage. Its `Theme`/`ThemeColor` (`crates/ui/src/
//      theme/`) has ~100 individual `Hsla` fields — comprehensive for
//      colors — but only two radius slots (`radius`, `radius_lg`) against
//      MDS's 5-tier scale (`theme.rs`'s `RADIUS_SM/MD/LG/XL/4XL`). Per-
//      component radius (badges' pill `4xl`, buttons' `lg`, cards' `xl`)
//      has no first-class override path in the global theme struct.
//   3. Dependency-weight mismatch for the ask. The default build pulls
//      `crates/ui` + `crates/story` + `crates/assets` (875 packages
//      resolved in the spike's lockfile) for 9 simple components — a large,
//      actively-changing dependency surface with its own theming engine,
//      icon set, and dock/chart/menu/highlighter modules this task doesn't
//      need.
//
// One genuinely good finding, worth keeping on record: `crates/base/src/
// input/base/state.rs` (NOT `crates/ui/src/input/` — a separate module the
// spike almost missed) implements a real, unit-tested `EntityInputHandler`
// IME composition path (`replace_and_mark_text_in_range` / `unmark_text`,
// tests exercise Japanese hiragana "あ" and Chinese pinyin "n"→"ni"→"你").
// `text_field.rs`'s doc comment already flags the lack of IME support as a
// known gap for CJK input — S4's chat composer will need real composition,
// and vendoring just that one module (not the whole design system) is a
// narrow, worth-revisiting follow-up once the rev-pin problem above has a
// real answer. Not attempted in this pass.
//
// Every component below is therefore hand-rolled `div()` composition.

pub mod badge;
pub mod button;
pub mod card;
pub mod dialog;
pub mod empty_state;
pub mod skeleton;
pub mod table;
pub mod tabs;
pub mod toast;

pub use badge::{badge, BadgeVariant};
pub use button::{button, ButtonVariant};
pub use card::{card, card_content, card_description, card_footer, card_header, card_title};
pub use dialog::{dialog_button_row, dialog_panel};
// `dialog_overlay` / `toast_container` are the real-usage (full-screen /
// fixed-position) wrappers — `screens/gallery.rs` deliberately renders
// `dialog_panel`/`toast` inline as static previews instead (see those
// modules' doc comments), so nothing in this pass calls either yet. Kept
// `pub` as forward-looking facade surface for the first real caller.
#[allow(unused_imports)]
pub use dialog::dialog_overlay;
pub use empty_state::empty_state;
pub use skeleton::skeleton;
pub use table::table;
pub use tabs::{tabs, TabItem};
pub use toast::{toast, ToastVariant};
#[allow(unused_imports)]
pub use toast::toast_container;
