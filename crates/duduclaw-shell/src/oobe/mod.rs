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
mod network;
mod network_ui;
mod persistence;
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
pub use persistence::{boot_theme, load_state, resolve_boot_flow, save_state, state_path};
/// Ephemeral UI-only state — see `ui_state.rs`'s own header comment.
pub use ui_state::{AccountClaimFailureKind, AccountClaimState, OobeUiState};
