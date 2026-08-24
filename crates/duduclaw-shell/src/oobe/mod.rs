// OOBE (Out-of-Box Experience) state machine + persistence — Shell-S1
// (2026-08-20).
//
// Round 1 shipped the full nine-step SKELETON (state machine + persistence
// + boot-entry resolution) plus REAL screens for the first five steps, with
// steps 6-9 (RuntimeAuth/Privacy/Templates/Finish) rendering as an honest
// placeholder page. Round 2 (this pass) replaces that placeholder with real
// screens for those four steps (`steps::{runtime_auth,privacy,templates,
// finish}`) and hardens the on-disk schema for forward compatibility (see
// the `#[serde(default)]` note on `OobeState`/`OobeSelections` below) — the
// STATE MACHINE rules for all nine steps were already real and enforced
// since round 1, only the visual content and the new selection fields those
// four steps need (`runtime_authorized`, the four `privacy_*` toggles, and
// `TemplateChoice::Custom`) are new this round.
//
// Step order/skippability is NOT invented — it is taken (with ONE correction,
// see below) from `research/native-os-2026-08/oobe-first-run-reference.md`
// §B-1's "值班機（kiosk）建議流程" table (device-type OOBE camp: macOS/
// Windows/ChromeOS/Steam Deck — DuDuClaw OS is device-type per that doc's own
// classification in §A row 7: "裝置型全做——DuDuClaw OS 屬裝置型"). Where the
// task brief itself already settled a judgment call (the Network step:
// blocking, no "later" escape hatch — see the brief's own parenthetical
// reasoning), that judgment is followed as-is. Where neither the brief nor the
// research doc says anything about skippability, the default is NOT
// skippable ("不確定就從嚴=不可跳"), and each `OobeStep` variant's own doc
// comment below names its citation (or the from-the-strict-default fallback)
// explicitly.
//
// ── Correction (this round): language leads, not input detection ─────────
// The FIRST implementation of this file put `InputDetection` at step 0 and
// `LanguageAccessibility` at step 1 — a literal transcription of §B-1's table
// ROW numbers (row 0 = input detection, row 1 = language). That was an
// implementation slip: it contradicts the SAME research doc's own strongest
// finding, §A consensus #1 ("語言第一屏，在一切帳號/授權之前" — 6/8, the
// loudest cross-OS agreement in the whole survey, ahead of even
// network/privacy) and §B-4 point 4 ("原生把語言鎖第一屏、確定後再渲染其餘
// 步驟；設計期就決定支不支援中途換語言" — explicitly named as a thing the
// web port must NOT get wrong). This round's task brief settles it in plain
// language ("OOBE 第一步應該是要先選語言，接下來才用選擇的語言去繪製下面的
// 頁面") and this file is corrected to match: `LanguageAccessibility` is now
// `OobeStep::ALL[0]`, `InputDetection` is `ALL[1]`. Every OTHER row's
// relative order from §B-1 is unchanged (Network still directly precedes
// Update, etc.) — only where language sits moved, from second to first.
//
// Pure `&mut self` mutation, no gpui types anywhere in this module's files
// (state.rs/selections.rs/persistence.rs/ui_state.rs, split out of this
// file below) — same "testable without a live window" discipline
// `surface.rs`'s header comment establishes for `SurfaceState`, extended
// here to also cover disk persistence (still plain data in, plain data out,
// no gpui).
//
// ── WP-OOBE-split (2026-08-21): four files, one module ────────────────────
// This file had grown to 2,030 lines — well past this crate's own <800-line
// file convention — so its content now lives in four siblings: `state.rs`
// (`OobeStep`/`OobeFlow` — the state machine itself), `selections.rs`
// (`OobeState`/`OobeSelections` and the four selection-value enums:
// `LanguageChoice`/`TemplateChoice`/`ThemeChoice`/`PrivacyToggle`),
// `persistence.rs` (disk I/O + boot-entry resolution: `load_state`/
// `save_state`/`state_path`/`resolve_boot_flow`/`boot_theme`), and
// `ui_state.rs` (`OobeUiState` + the two `AccountClaim*` enums) — the same
// "split out, re-export so call sites don't change" shape `network_ui.rs`
// already established a round earlier for the `Network` step's own
// ephemeral enums (see that file's own header comment). Every design
// citation above is still exactly what motivated the code in all four
// files — it stayed here, at the top of the module, rather than being torn
// apart paragraph-by-paragraph across siblings that didn't exist when it
// was written; each sibling's own header comment points back here instead
// of repeating it. Zero behavior change: every `pub` item that used to be
// reachable as `oobe::X` still is, via the `pub use` list below; the only
// visibility WIDENING the split required is `OobeFlow::palette()` (see
// that method's own doc comment, now in `state.rs`, for why).

mod claim;
mod fake_data;
mod focus_order;
// W7-3 (2026-08-24, `IME-account-fields-zhuyin`): proactive fcitx5 engine
// switch on focus of ASCII-only fields — see its own header comment.
mod ime_focus;
mod network;
mod network_ui;
mod persistence;
// D4a §6 (2026-08-23): validates and opens a captive-portal sign-in URL.
// Its own header comment explains why an attacker-supplied URL needs a
// whole module rather than one `Command::new` call.
mod portal_browser;
mod render;
mod selections;
mod state;
mod steps;
mod ui_state;
mod widgets;

/// `main.rs` needs `AccountFields` to create the `AccountCreate` step's two
/// real text-input entities at window-open time and store them on
/// `ShellView` — see that struct's own doc comment in `widgets.rs`. Same
/// re-export shape `pub use render::render;` below already establishes:
/// `widgets` itself stays a private module, only specific items are opened
/// up.
pub(crate) use widgets::AccountFields;
/// `main.rs` needs `NetworkFields` for the same reason it needs
/// `AccountFields` (see that type's doc comment just above) — the
/// `Network` step's PSK entry (Shell-S3) is the second, and so far only
/// other, OOBE screen with a real typed field.
pub(crate) use widgets::NetworkFields;
/// `crate::lockscreen` needs `LockPasswordField` for the same reason
/// `main.rs` needs `AccountFields`/`NetworkFields` above — see that type's
/// own doc comment in `widgets.rs` (WP-lock-pw, 2026-08-22).
pub(crate) use widgets::LockPasswordField;
/// `crate::overlay::launcher` needs `LauncherQueryField` for the same reason
/// again (D3-b, 2026-08-23) — the Launcher's search box became a real
/// IME-capable field instead of a `String` appended to by a root key
/// listener. See that type's own doc comment in `widgets.rs`.
pub(crate) use widgets::LauncherQueryField;
/// `crate::settings` needs `SettingsFields` for the same reason again (D4b,
/// 2026-08-23) — the 系統設定 app's eight typed fields (a timezone, three
/// password halves, a Wi-Fi passphrase and a three-part static-IP form). See
/// that type's own doc comment in `widgets.rs` for why one bundle rather
/// than three, and why it is defined under `oobe` despite having nothing to
/// do with the OOBE flow.
pub(crate) use widgets::SettingsFields;
/// The widget type itself, under the name its non-OOBE consumers use. The
/// settings pages build their own labelled rows around individual fields
/// (`Entity<SettingsTextField>` in a helper signature), which `AccountFields`'
/// consumers do too — they just happen to sit inside `oobe` and can name
/// `widgets::OobeTextField` directly. An alias, not a second type: it IS the
/// same widget, and calling it `OobeTextField` at a settings call site would
/// wrongly imply otherwise.
pub(crate) use widgets::OobeTextField as SettingsTextField;
/// `NetScanState`/`NetConnectState`/`NetConnectFailureKind` live in their
/// own file (`network_ui.rs` — see that file's own header comment for why)
/// but are re-exported here so every call site keeps addressing them as
/// `oobe::NetScanState` etc., unchanged — `steps::network` and
/// `ui_state.rs`'s own `OobeUiState`/tests are the only consumers, and none
/// of them need to know the definitions moved.
pub use network_ui::{NetConnectFailureKind, NetConnectState, NetScanState};

pub use render::render;

/// The state machine itself (`OobeStep`/`OobeFlow`) — see `state.rs`'s own
/// header comment for why it's a separate file from the persisted data
/// shape it mutates.
pub use state::{OobeFlow, OobeStep};
/// WP-oobe-enter (2026-08-23): the pure "what should Enter do on this
/// step" decision (`OobeFlow::enter_outcome`, `state.rs`) plus the one
/// function that ACTS on its two `Submit*` variants (`steps::
/// handle_enter_submit`, itself dispatching to `steps::account`/`steps::
/// network`'s own submit fns) — `main.rs`'s `on_oobe_next` (Enter's
/// action handler) is the only caller of either. `handle_enter_submit`
/// re-exported from the otherwise-private `steps` module the same way
/// `AccountFields`/`NetworkFields` above are re-exported from the
/// otherwise-private `widgets` module.
pub(crate) use state::EnterOutcome;
pub(crate) use steps::handle_enter_submit;
/// WP-oobe-tab (2026-08-23): per-step Tab/Shift-Tab focus order — see
/// `focus_order.rs`'s own header comment. `main.rs`'s `on_focus_next`/`on_
/// focus_prev` action handlers are the only callers.
pub(crate) use focus_order::{focus_next, focus_order, focus_prev, OobeFocusTarget};
/// The persisted selection shape + its four value enums — see
/// `selections.rs`'s own header comment. `OobeSelections`/`OobeState`
/// themselves have no direct call site outside `oobe`'s own files (every
/// external reader goes through `OobeFlow::state()`/`selections()`
/// instead) — same true before this split, when they were direct `pub`
/// definitions here rather than a re-export; a bare `pub struct` is exempt
/// from `dead_code`, but `pub use` of an otherwise-unreferenced name isn't
/// exempt from `unused_imports`, so the `#[allow]` below is this split's
/// own artifact, not a new reachability decision — the path stays exactly
/// as public as it always was.
#[allow(unused_imports)]
pub use selections::{LanguageChoice, OobeSelections, OobeState, PrivacyToggle, TemplateChoice, ThemeChoice};
/// Disk I/O + boot-entry resolution — see `persistence.rs`'s own header
/// comment. `state_path()` has no direct call site outside `oobe`'s own
/// files either (same `#[allow]` reasoning as `OobeSelections`/`OobeState`
/// just above).
#[allow(unused_imports)]
pub use persistence::{boot_operator_name, boot_theme, load_state, resolve_boot_flow, save_state, state_path};
/// Ephemeral UI-only state — see `ui_state.rs`'s own header comment.
pub use ui_state::{AccountClaimFailureKind, AccountClaimState, OobeUiState};
/// ICON-3 (2026-08-23): the language step's five accessibility categories,
/// surfaced here only so `crate::icons`' mapping tests can iterate them —
/// see `steps/mod.rs`'s own re-export comment. `#[allow(unused_imports)]`
/// for exactly the reason the `OobeSelections`/`state_path` re-exports above
/// carry it: the ONLY consumer is a `#[cfg(test)]` module, so a release
/// build legitimately sees no use of the path.
#[allow(unused_imports)]
pub(crate) use steps::A11yCategory;
