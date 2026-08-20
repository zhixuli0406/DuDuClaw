// S4 — IME-composition-capable multi-line text input for the chat composer.
//
// ── Why this exists instead of `text_field.rs` ───────────────────────────
// `text_field.rs` (S2, login screen) is explicitly scoped OUT of IME
// support — plain `on_key_down` printable-char appends, "composing CJK text
// into it will not work correctly" per its own doc comment. A chat composer
// for a zh-TW-first product cannot ship with that gap: typing 你好 via a
// pinyin IME needs the OS candidate-window protocol
// (`gpui::EntityInputHandler`'s `replace_and_mark_text_in_range`/
// `unmark_text`/marked-range reporting), not raw keystroke capture.
//
// ── Where this code actually comes from (D4 spike revisited) ─────────────
// `mds_gpui/mod.rs`'s S3 doc comment flagged `gpui-component`'s
// `crates/base/src/input/base/state.rs` as "a real, unit-tested
// `EntityInputHandler` IME composition path" worth narrowly vendoring later.
// This pass went to do exactly that — and found the premise didn't hold up
// under inspection of the actual current file (not guessed, read directly
// via the same local `~/.cargo/git/checkouts/gpui-component-*` spike
// checkout `mds_gpui/mod.rs` already used):
//
//   - `state.rs` alone is ~4,950 lines, not a small IME module. It is the
//     SHARED editing engine backing `gpui-component`'s Input/Textarea/Editor
//     ALL THREE — LSP completions, diagnostics, code folding, search,
//     undo/redo, syntax highlighting, and IME composition are one
//     inseparable implementation, not composable pieces.
//   - It depends on a dozen sibling private modules in the SAME crate
//     (`blink_cursor`, `cursor`, `element` (3,036 lines), `kind`, `layout`,
//     `mask_pattern`, `mode`, `movement`, `native`, `rope_ext`,
//     `undo_manager`) plus two substantial external crates (`ropey` for its
//     rope-based buffer, `sum_tree` for the rope's B-tree index).
//   - "只拿必要檔案，不拉整個 crate 依賴" (the task brief's own vendoring
//     constraint) is not achievable here: there IS no narrow slice. Pulling
//     just the IME methods out would still require carrying Selection/
//     Cursor/UndoManager types those methods touch, at which point the
//     "vendor" is most of the editing engine — exactly the dependency-
//     weight problem the S3 D4 spike already rejected `gpui-component` FOR
//     (see `mds_gpui/mod.rs`'s point 3), now recurring at the single-file
//     level instead of the whole-crate level.
//
// This is real new information from actually opening the file, not a
// reversal of judgment without cause — so this pass follows the task
// brief's OWN documented fallback for an incompatible/impractical vendor:
// "改讀 zed repo 的 gpui input 範例自行實作 EntityInputHandler". Concretely,
// `zed-industries/zed`'s own `crates/gpui/examples/input.rs` (Apache-2.0,
// pinned rev `7a7c3e1d2f03195c5fa19bc890da330ad7f3abef` — verified against
// this crate's own `Cargo.lock` `gpui` entry, the EXACT commit already
// compiled into this binary, not "some recent version") implements the
// identical `EntityInputHandler` marked-text protocol `gpui-component`'s
// engine sits on top of, self-contained in ~400 lines of directly portable
// logic. `text_engine.rs` is a close port of that example's offset/range/
// composition methods (pulled out of its `gpui` `Entity` struct into a
// plain, gpui-free `TextEngine` so it's unit-testable without a live
// window/app — gpui's own IME example has no unit tests to "bring along";
// this module adds its own, covering the same composition sequences
// gpui-component's now-superseded target file was noted for exercising:
// multi-step pinyin marking→commit and single-step hiragana marking→unmark,
// see `text_engine.rs`'s tests). `element.rs` extends the example's single-
// line `TextElement` to multi-line (row-per-`\n`) layout/paint/hit-testing.
// `chat_input.rs` is the `Entity`/glue layer — key handling follows this
// CRATE's own `text_field.rs` convention (plain `on_key_down`), not the
// zed example's global `actions!`/`KeyBinding` registration.
//
// ── What is and isn't verified ────────────────────────────────────────────
// The composition ARITHMETIC (UTF-8⇄UTF-16 offset conversion, marked-range
// tracking, multi-step replace-and-mark sequences) is unit tested — see
// `text_engine.rs`. What is NOT and CANNOT be automated from here: an actual
// OS IME (macOS 注音/拼音輸入法, an OS-level component this test harness has
// no driver for) actually invoking `replace_and_mark_text_in_range` through
// real keystrokes and rendering the candidate window in the right screen
// position. That is flagged explicitly in this crate's S4 report as
// "待使用者親測" — not silently assumed to work.

mod chat_input;
mod element;
mod text_engine;

pub use chat_input::{ChatInputEvent, ChatInputState};
