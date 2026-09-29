//! Y10-1 — `duduclaw os audio <verb>`: the audio twin of `os_drive::display`.
//!
//! O16: a thin adapter over [`duduclaw_gateway::os_ops`]'s `audio_get` /
//! `audio_set`, which in turn wrap `duduclaw_gateway::audio_bridge` — the
//! same authority the `os_audio_get`/`os_audio_set` MCP tools reach.
//!
//! `os_drive::display` hand-rolls comp's socket wire protocol a second time
//! (once for the CALLING process's own `$XDG_RUNTIME_DIR`, once inside
//! `duduclaw_gateway::display_bridge` for the fixed kiosk path) because it
//! predates A7c and had to keep its already-shipped error text byte-for-byte
//! stable. Audio has no such legacy surface — `audio_bridge::run_wpctl`
//! already tries the calling process's own ambient environment first and
//! only falls back to the fixed kiosk path on failure, so one function
//! covers both attempts here (unlike `display`'s two duplicated ones).
//!
//! `finish()` in `os_drive/mod.rs` expects `Result<String, String>`, so every
//! function here renders the authority's `Value` down to a pretty-printed
//! string on success.

use duduclaw_gateway::os_ops;
use serde_json::Value;

fn render(result: Result<Value, os_ops::OsOpError>) -> Result<String, String> {
    match result {
        Ok(v) => Ok(format!("{v:#}")),
        Err(e) => Err(e.message()),
    }
}

pub async fn get() -> Result<String, String> {
    render(os_ops::audio_get().await)
}

pub async fn volume_set(pct: u8) -> Result<String, String> {
    render(os_ops::audio_set("volume", &pct.to_string()).await)
}

pub async fn mute_toggle() -> Result<String, String> {
    render(os_ops::audio_set("mute", "toggle").await)
}

pub async fn output_set(id: u32) -> Result<String, String> {
    render(os_ops::audio_set("output", &id.to_string()).await)
}
