// duduclaw-native-gui as a library — Shell-S0 (2026-08-19).
//
// Every module in this crate used to be private to its own `main.rs`
// binary. The new `duduclaw-shell` crate (DuDuClaw OS's desktop shell app)
// needs exactly two of them — the MDS token catalog (`theme.rs`) and the
// hand-rolled component facade (`mds_gpui/`) — to build a SECOND gpui
// binary without forking either module or vendoring a copy. This file is
// the minimal public surface for that: only `theme` and `mds_gpui` are
// `pub`; everything else (`api.rs`, `screens/`, `ws_status.rs`, `chat_ws.rs`,
// ...) stays private implementation detail of THIS crate's own
// `duduclaw-native-gui` binary and is not re-exported here.
//
// `main.rs` now reaches these two modules through THIS lib target
// (`use duduclaw_native_gui::{mds_gpui, theme};`) instead of its own
// `mod theme; mod mds_gpui;` declarations, so there is exactly ONE compiled
// copy of each — not a bin-local copy plus a lib-local copy silently
// diverging over time. Cargo wires the bin target against this crate's own
// lib target automatically (the standard `src/lib.rs` + `src/main.rs`
// pattern) — no extra `[dependencies]` entry needed in `Cargo.toml` for
// that self-reference.
// D3-b (2026-08-23): `ime_input` joins them. It was `main.rs`'s own private
// `mod ime_input;` until now; the shell's text fields need the identical
// `EntityInputHandler` composition path (see that module's own header
// comment for why the code stays HERE rather than moving to a third crate or
// being copied). Same one-compiled-copy rule as the two above: `main.rs`
// reaches it through this lib target, not through a bin-local `mod`.
pub mod ime_input;
pub mod mds_gpui;
pub mod theme;
