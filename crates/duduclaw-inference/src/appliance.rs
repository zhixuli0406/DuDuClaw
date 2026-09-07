//! Appliance-profile inference defaults — the DuDuClaw OS image ships
//! llama.cpp's `llama-server` and a unit that serves an OpenAI-compatible
//! API on loopback, so on that image local inference should work out of the
//! box without anyone hand-writing an `inference.toml`.
//!
//! ## What this module decides
//!
//! When BOTH of these hold:
//!
//! 1. [`duduclaw_core::appliance::is_appliance`] — the image's unit set
//!    `DUDUCLAW_APPLIANCE=1` on the gateway process, and
//! 2. [`LLAMA_SERVER_PATH`] exists on disk (the image actually shipped the
//!    binary — a platform build running with the env var set but without
//!    the binary must NOT pretend a local endpoint exists),
//!
//! …the keys the operator did not write into `inference.toml` are filled
//! in with the image's layout: `enabled = true`,
//! `backend = "openai_compat"`, `[openai_compat] base_url =`
//! [`LLAMA_SERVER_ENDPOINT`], and `models_dir = <DUDUCLAW_HOME>/models`.
//!
//! ## What it deliberately does NOT decide
//!
//! - **`[general] inference_mode` stays `hybrid`.** That key lives in
//!   `config.toml`, not here, and nothing in this module touches it: a
//!   configured cloud runtime must still win. The appliance default only
//!   makes the local path *available*, never mandatory.
//! - **It never overrides a key the operator wrote.** The overlay is a
//!   defaults layer applied to the parsed TOML table; every explicit value
//!   in the file survives verbatim, including `enabled = false`.
//! - **It never invents a model.** The endpoint being configured says
//!   nothing about whether a GGUF was ever downloaded;
//!   [`no_local_model`] is the honest "nothing installed yet" check callers
//!   use before making a doomed request.
//!
//! The paths and the env-file variable names are pinned by the OS image's
//! recipe (`duduclaw-llama-server.service`, written by WP-F in the
//! DuDuClaw-OS repo) and must not change here without changing it there.

use std::path::Path;

/// Absolute path the OS image installs llama.cpp's server binary at.
/// Pinned by the image recipe — do not change unilaterally.
pub const LLAMA_SERVER_PATH: &str = "/usr/bin/llama-server";

/// OpenAI-compatible base URL the image's unit serves on.
pub const LLAMA_SERVER_ENDPOINT: &str = "http://127.0.0.1:8080/v1";

/// Default port the image's unit listens on (`LLAMA_PORT` in the env file).
pub const LLAMA_SERVER_PORT: u16 = 8080;

/// Env file the image's unit reads (`EnvironmentFile=`), relative to
/// `DUDUCLAW_HOME` (`/data/duduclaw/llama-server.env` on the appliance).
pub const LLAMA_SERVER_ENV_FILE: &str = "llama-server.env";

/// Unit name the image installs. Only ever passed to the service manager on
/// an actual appliance; never shown to a person (see the dashboard's
/// `technical-terms` i18n guard).
pub const LLAMA_SERVER_UNIT: &str = "duduclaw-llama-server.service";

/// Model name reported to the OpenAI-compat endpoint. llama.cpp's server
/// answers regardless of the requested model id (it serves whichever GGUF
/// was loaded), so this is a stable placeholder rather than a claim about
/// which weights are live — ask [`LLAMA_SERVER_ENDPOINT`]`/models` for that.
pub const LOCAL_MODEL_ALIAS: &str = "local";

/// Default context length written into the env file when the caller does
/// not pick one. 4096 fits every catalog model on the target hardware
/// (N305 / 8845HS class, no discrete VRAM) without tuning.
pub const DEFAULT_CTX: u32 = 4096;

/// Pure: do the appliance inference defaults apply?
///
/// Both inputs must hold — see the module doc for why the binary check is
/// not redundant with the env flag.
pub fn appliance_defaults_apply(is_appliance: bool, llama_server_present: bool) -> bool {
    is_appliance && llama_server_present
}

/// Live version of [`appliance_defaults_apply`] against this process's env
/// and filesystem.
pub fn appliance_defaults_active() -> bool {
    appliance_defaults_apply(
        duduclaw_core::appliance::is_appliance(),
        Path::new(LLAMA_SERVER_PATH).exists(),
    )
}

/// Pure: overlay the home-relative `models_dir` default onto a parsed
/// `inference.toml` table.
///
/// Fixes the long-standing split between two conventions for "the models
/// directory": [`crate::config::InferenceConfig`]'s literal
/// `"~/.duduclaw/models"` default and the gateway's `<home>/models`
/// (`local_models.rs`, `models.list`). Those agree only while
/// `DUDUCLAW_HOME` is unset; with it set — which is exactly the appliance
/// case (`/data/duduclaw`) — the engine looked in `$HOME/.duduclaw/models`
/// while every download landed in `$DUDUCLAW_HOME/models`.
///
/// Applies on every host, appliance or not: an absent key means "wherever
/// this install keeps its state", which is the home dir it was loaded from.
/// An explicit `models_dir` in the file still wins.
pub fn overlay_models_dir_default(table: &mut toml::Table, models_dir: &str) {
    if !table.contains_key("models_dir") {
        table.insert(
            "models_dir".to_string(),
            toml::Value::String(models_dir.to_string()),
        );
    }
}

/// Pure: overlay the appliance defaults onto a parsed `inference.toml`
/// table. Only keys absent from the table are filled in.
///
/// `models_dir` is the caller's resolved `<DUDUCLAW_HOME>/models`.
pub fn overlay_appliance_defaults(table: &mut toml::Table, models_dir: &str) {
    overlay_models_dir_default(table, models_dir);

    if !table.contains_key("enabled") {
        table.insert("enabled".to_string(), toml::Value::Boolean(true));
    }
    if !table.contains_key("backend") {
        table.insert(
            "backend".to_string(),
            toml::Value::String("openai_compat".to_string()),
        );
    }
    if !table.contains_key("openai_compat") {
        let mut compat = toml::Table::new();
        compat.insert(
            "base_url".to_string(),
            toml::Value::String(LLAMA_SERVER_ENDPOINT.to_string()),
        );
        compat.insert(
            "model".to_string(),
            toml::Value::String(LOCAL_MODEL_ALIAS.to_string()),
        );
        table.insert("openai_compat".to_string(), toml::Value::Table(compat));
    }
}

/// Machine-readable marker prefix for "the local path is configured but no
/// weights are installed". Callers match on the prefix to distinguish this
/// from a genuine backend failure; the text after it is human-readable.
pub const NO_LOCAL_MODEL_MARKER: &str = "NO_LOCAL_MODEL";

/// Pure: is this the "configured endpoint, but nothing to serve" case?
///
/// Deliberately narrow. It answers `true` only when the appliance defaults
/// are what put the endpoint there AND the models directory holds no GGUF —
/// i.e. the one situation where a request is guaranteed to fail because
/// nothing was ever downloaded. An operator who pointed `[openai_compat]`
/// at their own server is never second-guessed by this, and a machine with
/// a GGUF present is never pre-emptively refused (the request may still
/// fail for other reasons, which is then reported as itself).
pub fn no_local_model(appliance_defaults: bool, models_dir_has_gguf: bool) -> bool {
    appliance_defaults && !models_dir_has_gguf
}

/// Does `dir` contain at least one `.gguf` file? Missing/unreadable
/// directory reads as "no" — the caller's next step is the honest
/// "nothing installed" message either way.
pub async fn models_dir_has_gguf(dir: &Path) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return false;
    };
    while let Ok(Some(e)) = entries.next_entry().await {
        if e.file_name().to_string_lossy().ends_with(".gguf") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_need_both_the_flag_and_the_binary() {
        assert!(appliance_defaults_apply(true, true));
        // Env var set on a platform build that has no llama-server: must not
        // claim a local endpoint exists.
        assert!(!appliance_defaults_apply(true, false));
        // Developer laptop that happens to have llama-server installed.
        assert!(!appliance_defaults_apply(false, true));
        assert!(!appliance_defaults_apply(false, false));
    }

    fn parse(s: &str) -> toml::Table {
        s.parse::<toml::Table>().expect("parse")
    }

    #[test]
    fn overlay_fills_every_absent_key() {
        let mut t = parse("");
        overlay_appliance_defaults(&mut t, "/data/duduclaw/models");
        assert_eq!(t["enabled"].as_bool(), Some(true));
        assert_eq!(t["backend"].as_str(), Some("openai_compat"));
        assert_eq!(t["models_dir"].as_str(), Some("/data/duduclaw/models"));
        assert_eq!(
            t["openai_compat"]["base_url"].as_str(),
            Some(LLAMA_SERVER_ENDPOINT)
        );
        assert_eq!(t["openai_compat"]["model"].as_str(), Some(LOCAL_MODEL_ALIAS));
    }

    #[test]
    fn overlay_never_overrides_an_explicit_value() {
        let mut t = parse(
            r#"
enabled = false
backend = "llama_cpp"
models_dir = "/mnt/big/models"

[openai_compat]
base_url = "http://10.0.0.5:9999/v1"
model = "my-model"
"#,
        );
        overlay_appliance_defaults(&mut t, "/data/duduclaw/models");
        // An operator who turned local inference off stays off.
        assert_eq!(t["enabled"].as_bool(), Some(false));
        assert_eq!(t["backend"].as_str(), Some("llama_cpp"));
        assert_eq!(t["models_dir"].as_str(), Some("/mnt/big/models"));
        assert_eq!(t["openai_compat"]["base_url"].as_str(), Some("http://10.0.0.5:9999/v1"));
    }

    #[test]
    fn overlay_fills_a_partial_file() {
        // Half-written file: keep what's there, fill the rest.
        let mut t = parse("enabled = true\n");
        overlay_appliance_defaults(&mut t, "/data/duduclaw/models");
        assert_eq!(t["enabled"].as_bool(), Some(true));
        assert_eq!(t["backend"].as_str(), Some("openai_compat"));
        assert!(t.contains_key("openai_compat"));
    }

    #[test]
    fn models_dir_overlay_is_independent_of_the_appliance() {
        let mut t = parse("enabled = true");
        overlay_models_dir_default(&mut t, "/home/kai/.duduclaw/models");
        assert_eq!(t["models_dir"].as_str(), Some("/home/kai/.duduclaw/models"));
        // No appliance keys were added by the models_dir-only overlay.
        assert!(!t.contains_key("backend"));
        assert!(!t.contains_key("openai_compat"));
    }

    #[test]
    fn no_local_model_only_fires_for_appliance_defaults_with_no_gguf() {
        assert!(no_local_model(true, false));
        assert!(!no_local_model(true, true));
        // Operator-configured endpoint: never second-guessed.
        assert!(!no_local_model(false, false));
        assert!(!no_local_model(false, true));
    }

    #[tokio::test]
    async fn gguf_scan_reports_honestly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!models_dir_has_gguf(dir.path()).await);
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        assert!(!models_dir_has_gguf(dir.path()).await);
        std::fs::write(dir.path().join("qwen3-1.7b-q4_k_m.gguf"), b"x").unwrap();
        assert!(models_dir_has_gguf(dir.path()).await);
        // Missing directory reads as "no", not as an error.
        assert!(!models_dir_has_gguf(&dir.path().join("nope")).await);
    }
}
