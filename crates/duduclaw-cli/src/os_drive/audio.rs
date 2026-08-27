//! Y10-1 — `duduclaw os audio <verb>`: the audio twin of `os_drive::display`,
//! but a THIN wrapper directly over `duduclaw_gateway::audio_bridge` rather
//! than a second hand-rolled client — see this module's doc for why.
//!
//! `os_drive::display` hand-rolls comp's socket wire protocol a second time
//! (once for the CALLING process's own `$XDG_RUNTIME_DIR`, once inside
//! `duduclaw_gateway::display_bridge` for the fixed kiosk path) because it
//! predates A7c and had to keep its already-shipped error text byte-for-byte
//! stable. Audio has no such legacy surface — this CLI group is introduced
//! in the SAME round as `duduclaw_gateway::audio_bridge` itself, which
//! already tries the calling process's own ambient environment first and
//! only falls back to the fixed kiosk path on failure (`audio_bridge::
//! run_wpctl`'s own two-tier retry — see that module's doc for why one
//! function does both attempts instead of `display`'s two independently
//! duplicated ones). Duplicating that retry AND the `wpctl status` tree
//! parser a third time here (this crate → gateway → shell would already be
//! two copies) would triple a genuinely fiddly parser for zero behavioral
//! gain — same "system"/"network" precedent `os_drive/mod.rs`'s own module
//! doc cites for calling gateway's pure functions directly rather than
//! re-implementing them.
//!
//! `finish()` in `os_drive/mod.rs` expects `Result<String, String>`, so every
//! function here renders `duduclaw_gateway::audio_bridge`'s `Result<Value,
//! String>` down to a pretty-printed string on success.

use serde_json::Value;

fn render(v: Value) -> String {
    format!("{v:#}")
}

pub async fn get() -> Result<String, String> {
    duduclaw_gateway::audio_bridge::audio_get().await.map(render)
}

pub async fn volume_set(pct: u8) -> Result<String, String> {
    duduclaw_gateway::audio_bridge::audio_set("volume", &pct.to_string()).await.map(render)
}

pub async fn mute_toggle() -> Result<String, String> {
    duduclaw_gateway::audio_bridge::audio_set("mute", "toggle").await.map(render)
}

pub async fn output_set(id: u32) -> Result<String, String> {
    duduclaw_gateway::audio_bridge::audio_set("output", &id.to_string()).await.map(render)
}
