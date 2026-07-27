//! Photo → desktop-pet Tauri commands (WP-P2/P3).
//!
//! Thin bridge over the `duduclaw-pets` crate (which owns the pack format, disk
//! layout, slug rules, and background removal). These commands are the desktop
//! shell's only pet surface; the studio page and the mascot overlay call them via
//! `window.__TAURI__.core.invoke(...)`.
//!
//! Background removal runs locally: BiRefNet-general-lite (primary) → silueta
//! (fallback) → passthrough (no model / external cutout). The original photo is
//! retained for regeneration.
//!
//! NOTE: like the rest of the desktop shell, this targets the Tauri 2 API but is
//! not compiled in this environment (no Tauri toolchain).

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use duduclaw_pets::{
    BackgroundRemover, OnnxRemover, PassthroughRemover, PetMode, PetPack, BIREFNET_LITE, SILUETA,
};

use crate::mascot_window;

/// Event broadcast to all windows when the active pet changes (the overlay
/// listens and re-fetches). Kept as a string constant so JS and Rust agree.
pub const PET_CHANGED_EVENT: &str = "pet://changed";

/// A pet as shown in the studio list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetSummary {
    pub slug: String,
    pub display_name: String,
    pub mode: String,
    pub active: bool,
}

/// Everything the overlay runtime needs to render + animate a pet.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetRuntimePayload {
    pub slug: String,
    pub display_name: String,
    pub mode: String,
    /// The renderable image as a `data:` URL (procedural cutout or spritesheet).
    pub image_data_url: Option<String>,
    /// Behavior weights (state → frequency) the runtime uses for idle variants.
    pub behaviors: Vec<PetBehavior>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PetBehavior {
    pub state: String,
    pub frequency: f32,
    pub condition: Option<String>,
}

/// Result of generating a pet: its summary plus a preview of the cutout and
/// which remover ran (so the UI can warn when the background was NOT removed).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedPet {
    pub slug: String,
    pub display_name: String,
    pub mode: String,
    /// The cutout preview as a `data:` URL.
    pub image_data_url: Option<String>,
    /// "birefnet" / "silueta" / "passthrough" — passthrough means no removal ran.
    pub remover_label: String,
}

/// Which local models are installed (drives the studio's setup hints).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub birefnet_present: bool,
    pub silueta_present: bool,
    pub birefnet_url: String,
    pub silueta_url: String,
}

fn mode_str(mode: PetMode) -> String {
    match mode {
        PetMode::Sprite => "sprite",
        PetMode::Procedural => "procedural",
    }
    .to_string()
}

fn to_summary(pack: &PetPack, active_slug: Option<&str>) -> PetSummary {
    PetSummary {
        slug: pack.slug.clone(),
        display_name: pack.manifest.display_name.clone(),
        mode: mode_str(pack.manifest.mode),
        active: active_slug == Some(pack.slug.as_str()),
    }
}

/// Pick the best available background remover.
///
/// `external` = the caller supplies an already-cut-out PNG (store verbatim).
/// Otherwise prefer BiRefNet, then silueta, then degrade to passthrough (which
/// keeps the whole photo — honest "model not installed" behavior; the returned
/// label lets the UI tell the user the background was not removed).
fn pick_remover(external: bool) -> Box<dyn BackgroundRemover> {
    if external {
        return Box::new(PassthroughRemover);
    }
    if BIREFNET_LITE.is_present() {
        match OnnxRemover::load(&BIREFNET_LITE) {
            Ok(r) => return Box::new(r),
            Err(e) => tracing::warn!(error = %e, "BiRefNet load failed; trying silueta"),
        }
    }
    if SILUETA.is_present() {
        match OnnxRemover::load(&SILUETA) {
            Ok(r) => return Box::new(r),
            Err(e) => tracing::warn!(error = %e, "silueta load failed; using passthrough"),
        }
    }
    tracing::warn!("no segmentation model installed; storing photo without background removal");
    Box::new(PassthroughRemover)
}

/// List all pet packs.
#[tauri::command]
pub fn pet_list() -> Vec<PetSummary> {
    let active = duduclaw_pets::get_active_slug();
    duduclaw_pets::list_packs()
        .iter()
        .map(|p| to_summary(p, active.as_deref()))
        .collect()
}

/// Generate a pet from a base64-encoded photo.
///
/// `photo_base64` may be a bare base64 string or a `data:` URL (the prefix is
/// stripped). `external_cutout = true` skips segmentation (the PNG is already
/// transparent). Returns the new pet's summary. Does NOT auto-activate — the UI
/// activates explicitly after the user confirms the preview.
#[tauri::command]
pub fn pet_generate(
    name: String,
    photo_base64: String,
    external_cutout: bool,
) -> Result<GeneratedPet, String> {
    let raw = strip_data_url(&photo_base64);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.as_bytes())
        .map_err(|e| format!("invalid image data: {e}"))?;
    let remover = pick_remover(external_cutout);
    let label = remover.label().to_string();
    let pack = duduclaw_pets::generate_procedural_pet(&name, &bytes, remover.as_ref())
        .map_err(|e| e.to_string())?;
    tracing::info!(slug = %pack.slug, remover = %label, "generated pet pack");
    let image_data_url = pack.image_path().and_then(|p| encode_image_data_url(&p));
    Ok(GeneratedPet {
        slug: pack.slug,
        display_name: pack.manifest.display_name,
        mode: mode_str(pack.manifest.mode),
        image_data_url,
        remover_label: label,
    })
}

/// Strip a leading `data:*;base64,` prefix if present, returning the base64 body.
fn strip_data_url(s: &str) -> &str {
    if let Some(idx) = s.find("base64,") {
        &s[idx + "base64,".len()..]
    } else {
        s
    }
}

/// The currently active pet, if any.
#[tauri::command]
pub fn pet_active_get() -> Option<PetSummary> {
    let slug = duduclaw_pets::get_active_slug()?;
    let pack = duduclaw_pets::load_pack(&slug).ok()?;
    Some(to_summary(&pack, Some(&slug)))
}

/// Activate a pet (or clear with `null`), show the pet window, and broadcast the
/// change so an open overlay re-fetches.
#[tauri::command]
pub fn pet_activate(app: AppHandle, slug: Option<String>) -> Result<(), String> {
    duduclaw_pets::set_active_slug(slug.as_deref()).map_err(|e| e.to_string())?;
    if slug.is_some() {
        // Make sure the pet window exists and is visible.
        if let Err(e) = mascot_window::build_mascot_window(&app) {
            tracing::warn!(error = %e, "pet window build failed on activate");
        }
        if let Some(win) = tauri::Manager::get_webview_window(&app, mascot_window::MASCOT_LABEL) {
            let _ = win.show();
        }
    }
    let _ = app.emit(PET_CHANGED_EVENT, slug);
    Ok(())
}

/// Delete a pet pack.
#[tauri::command]
pub fn pet_remove(app: AppHandle, slug: String) -> Result<(), String> {
    duduclaw_pets::delete_pack(&slug).map_err(|e| e.to_string())?;
    let _ = app.emit(PET_CHANGED_EVENT, Option::<String>::None);
    Ok(())
}

/// Load the active pet's full runtime payload (manifest + image data URL).
#[tauri::command]
pub fn pet_load_active() -> Option<PetRuntimePayload> {
    let pack = duduclaw_pets::load_active()?;
    let image_data_url = pack.image_path().and_then(|p| encode_image_data_url(&p));
    let behaviors = pack
        .manifest
        .behaviors
        .iter()
        .map(|b| PetBehavior {
            state: b.state.clone(),
            frequency: b.frequency,
            condition: b.condition.clone(),
        })
        .collect();
    Some(PetRuntimePayload {
        slug: pack.slug,
        display_name: pack.manifest.display_name,
        mode: mode_str(pack.manifest.mode),
        image_data_url,
        behaviors,
    })
}

/// Read an image file and return it as a `data:` URL (PNG/WebP inferred by ext).
fn encode_image_data_url(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("webp") => "image/webp",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/png",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

/// Which local segmentation models are installed.
#[tauri::command]
pub fn pet_model_status() -> ModelStatus {
    ModelStatus {
        birefnet_present: BIREFNET_LITE.is_present(),
        silueta_present: SILUETA.is_present(),
        birefnet_url: BIREFNET_LITE.url.to_string(),
        silueta_url: SILUETA.url.to_string(),
    }
}

/// Download a segmentation model to `~/.duduclaw/models/`. `variant` is
/// `"birefnet"` or `"silueta"`. Returns the downloaded file's SHA-256. Runs on a
/// blocking thread (network + disk). NOT triggered at build time.
#[tauri::command]
pub async fn pet_model_download(variant: String) -> Result<String, String> {
    let spec = match variant.as_str() {
        "birefnet" => &BIREFNET_LITE,
        "silueta" => &SILUETA,
        other => return Err(format!("unknown model variant: {other}")),
    };
    tauri::async_runtime::spawn_blocking(move || {
        duduclaw_pets::download_model(spec).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("download task failed: {e}"))?
}
