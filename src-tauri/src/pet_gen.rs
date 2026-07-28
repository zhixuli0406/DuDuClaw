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
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, WebviewWindow};

use duduclaw_pets::{
    BackgroundRemover, OnnxRemover, PassthroughRemover, PetMode, PetPack, BIREFNET_LITE, SILUETA,
};

use crate::mascot_window;

/// Event broadcast to all windows when the active pet changes (the overlay
/// listens and re-fetches). Kept as a string constant so JS and Rust agree.
pub const PET_CHANGED_EVENT: &str = "pet://changed";

/// Event the main window listens for to navigate to the pet studio (fired by the
/// pet's right-click "open studio" item).
pub const PET_OPEN_STUDIO_EVENT: &str = "pet://open-studio";

/// Base (standard) desktop-pet window side in logical px — matches the initial
/// `inner_size` in `mascot_window.rs`. Small = 50%, large = 150%.
const PET_BASE_PX: f64 = 180.0;

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
    /// Sprite grid metadata (sprite mode only) — lets the runtime play frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprite_sheet: Option<SpriteSheet>,
}

/// Spritesheet playback metadata for sprite-mode pets.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteSheet {
    /// Frame cell width in px.
    pub frame_width: u32,
    /// Frame cell height in px.
    pub frame_height: u32,
    /// Frames per animation row.
    pub cols: u32,
    /// Per-state animation rows.
    pub animations: Vec<SpriteAnimation>,
}

/// One playable animation row (sprite mode).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteAnimation {
    pub state: String,
    pub row: u32,
    pub frames: u32,
    pub fps: u32,
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
/// transparent). `pixelate = true` (the default the studio sends) runs the local
/// pixel-art pipeline and bakes an animated Codex Pets spritesheet; `false`
/// keeps the single-image procedural pet. Returns the new pet's summary. Does
/// NOT auto-activate — the UI activates explicitly after the user confirms.
#[tauri::command]
pub fn pet_generate(
    name: String,
    photo_base64: String,
    external_cutout: bool,
    pixelate: bool,
) -> Result<GeneratedPet, String> {
    let raw = strip_data_url(&photo_base64);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.as_bytes())
        .map_err(|e| format!("invalid image data: {e}"))?;
    let remover = pick_remover(external_cutout);
    let label = remover.label().to_string();
    let pack = if pixelate {
        duduclaw_pets::generate_pixel_sprite_pet(&name, &bytes, remover.as_ref())
    } else {
        duduclaw_pets::generate_procedural_pet(&name, &bytes, remover.as_ref())
    }
    .map_err(|e| e.to_string())?;
    tracing::info!(slug = %pack.slug, remover = %label, pixelate, "generated pet pack");
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
        // Honor the saved size preference each time the pet is put on the desk.
        apply_saved_scale(&app);
    } else {
        // Deactivate = take the pet off the desk: hide the pet window, or the
        // pet keeps floating with no way to dismiss it.
        if let Some(win) = tauri::Manager::get_webview_window(&app, mascot_window::MASCOT_LABEL) {
            let _ = win.hide();
        }
    }
    let _ = app.emit(PET_CHANGED_EVENT, slug);
    Ok(())
}

/// Delete a pet pack. Removing the currently active pet also hides the pet
/// window (same rationale as deactivating).
#[tauri::command]
pub fn pet_remove(app: AppHandle, slug: String) -> Result<(), String> {
    let was_active = duduclaw_pets::load_active()
        .map(|p| p.manifest.id == slug)
        .unwrap_or(false);
    duduclaw_pets::delete_pack(&slug).map_err(|e| e.to_string())?;
    if was_active {
        if let Some(win) = tauri::Manager::get_webview_window(&app, mascot_window::MASCOT_LABEL) {
            let _ = win.hide();
        }
    }
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
    // Sprite-mode pets carry grid metadata so the runtime can play frames.
    let sprite_sheet = if pack.manifest.mode == PetMode::Sprite {
        let animations = pack
            .manifest
            .animations
            .iter()
            .map(|(state, a)| SpriteAnimation {
                state: state.clone(),
                row: a.row,
                frames: a.frames,
                fps: a.fps,
            })
            .collect();
        Some(SpriteSheet {
            frame_width: duduclaw_pets::sprite_bake::FRAME_W,
            frame_height: duduclaw_pets::sprite_bake::FRAME_H,
            cols: duduclaw_pets::sprite_bake::COLS,
            animations,
        })
    } else {
        None
    };
    Some(PetRuntimePayload {
        slug: pack.slug,
        display_name: pack.manifest.display_name,
        mode: mode_str(pack.manifest.mode),
        image_data_url,
        behaviors,
        sprite_sheet,
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

// ── Desktop-pet size (scale) persistence ─────────────────────────────────────

/// Persisted desktop-pet preferences (currently just the window scale).
#[derive(Debug, Serialize, Deserialize, Default)]
struct DesktopSettings {
    /// `"small"` | `"standard"` | `"large"`. Absent ⇒ standard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scale: Option<String>,
}

fn desktop_settings_path() -> std::path::PathBuf {
    duduclaw_pets::pets_dir().join("desktop.json")
}

/// The saved scale keyword, defaulting to `"standard"`.
fn load_scale() -> String {
    std::fs::read(desktop_settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<DesktopSettings>(&b).ok())
        .and_then(|s| s.scale)
        .unwrap_or_else(|| "standard".to_string())
}

fn save_scale(scale: &str) {
    let _ = std::fs::create_dir_all(duduclaw_pets::pets_dir());
    let json = serde_json::to_vec_pretty(&DesktopSettings {
        scale: Some(scale.to_string()),
    })
    .unwrap_or_default();
    let _ = std::fs::write(desktop_settings_path(), json);
}

/// Logical window side (px) for a scale keyword.
fn scale_to_px(scale: &str) -> f64 {
    match scale {
        "small" => PET_BASE_PX * 0.5,
        "large" => PET_BASE_PX * 1.5,
        _ => PET_BASE_PX,
    }
}

/// Resize the pet window to `scale` (does not persist).
fn resize_pet_window(app: &AppHandle, scale: &str) {
    if let Some(win) = app.get_webview_window(mascot_window::MASCOT_LABEL) {
        let px = scale_to_px(scale);
        let _ = win.set_size(LogicalSize::new(px, px));
    }
}

/// Apply the saved scale to the pet window (called when it is shown).
pub fn apply_saved_scale(app: &AppHandle) {
    resize_pet_window(app, &load_scale());
}

/// Set the desktop-pet window scale (`"small"` | `"standard"` | `"large"`),
/// persist it, and resize the live window immediately.
#[tauri::command]
pub fn pet_set_scale(app: AppHandle, scale: String) -> Result<(), String> {
    if !matches!(scale.as_str(), "small" | "standard" | "large") {
        return Err(format!("unknown scale: {scale}"));
    }
    save_scale(&scale);
    resize_pet_window(&app, &scale);
    Ok(())
}

/// The current desktop-pet scale keyword.
#[tauri::command]
pub fn pet_get_scale() -> String {
    load_scale()
}

/// Nudge the pet window horizontally by `dx` logical px (the autonomous walk).
/// Clamped to the current monitor's bounds so the pet never wanders off-screen.
/// Returns `true` when the move was clamped (hit a screen edge) — the runtime
/// uses that to turn the walk around.
#[tauri::command]
pub fn pet_move_by(app: AppHandle, dx: f64) -> Result<bool, String> {
    let Some(win) = app.get_webview_window(mascot_window::MASCOT_LABEL) else {
        return Ok(true); // no window — treat as blocked so the walk stops
    };
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let size = win.outer_size().map_err(|e| e.to_string())?;
    let scale = win.scale_factor().unwrap_or(1.0);
    let desired = pos.x + (dx * scale).round() as i32;
    let clamped = match win.current_monitor().ok().flatten() {
        Some(mon) => {
            let min_x = mon.position().x;
            let max_x = mon.position().x + mon.size().width as i32 - size.width as i32;
            desired.clamp(min_x, max_x.max(min_x))
        }
        None => desired,
    };
    win.set_position(tauri::PhysicalPosition::new(clamped, pos.y))
        .map_err(|e| e.to_string())?;
    Ok(clamped != desired)
}

/// Show the main window and ask it to navigate to the pet studio.
#[tauri::command]
pub fn pet_open_studio(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
    let _ = app.emit(PET_OPEN_STUDIO_EVENT, ());
}

// ── Right-click context menu ─────────────────────────────────────────────────

/// Pop up the desktop-pet's native right-click menu at the cursor.
///
/// Items: 收回桌面 · 切換寵物 (submenu of packs) · 大小 (小/標準/大) ·
/// 打開桌寵工作室. Menu clicks are routed by [`handle_pet_menu_event`] via the
/// app-level `on_menu_event` handler in `main.rs`.
#[tauri::command]
pub fn pet_context_menu(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let map_err = |e: tauri::Error| e.to_string();

    let hide =
        MenuItem::with_id(&app, "pet_hide", "收回桌面", true, None::<&str>).map_err(map_err)?;

    // Switch-pet submenu: every pack, the active one dotted.
    let active = duduclaw_pets::get_active_slug();
    let switch = Submenu::with_id(&app, "pet_switch_menu", "切換寵物", true).map_err(map_err)?;
    let packs = duduclaw_pets::list_packs();
    if packs.is_empty() {
        let none = MenuItem::with_id(&app, "pet_switch_none", "（尚無寵物）", false, None::<&str>)
            .map_err(map_err)?;
        switch.append(&none).map_err(map_err)?;
    } else {
        for p in &packs {
            let is_active = active.as_deref() == Some(p.slug.as_str());
            let label = if is_active {
                format!("● {}", p.manifest.display_name)
            } else {
                format!("　{}", p.manifest.display_name)
            };
            let item = MenuItem::with_id(
                &app,
                format!("pet_switch:{}", p.slug),
                label,
                !is_active,
                None::<&str>,
            )
            .map_err(map_err)?;
            switch.append(&item).map_err(map_err)?;
        }
    }

    // Size submenu, current scale dotted.
    let current = load_scale();
    let size = Submenu::with_id(&app, "pet_size_menu", "大小", true).map_err(map_err)?;
    for (key, label) in [
        ("small", "小 (50%)"),
        ("standard", "標準"),
        ("large", "大 (150%)"),
    ] {
        let dotted = if current == key {
            format!("● {label}")
        } else {
            format!("　{label}")
        };
        let item = MenuItem::with_id(&app, format!("pet_size:{key}"), dotted, true, None::<&str>)
            .map_err(map_err)?;
        size.append(&item).map_err(map_err)?;
    }

    let sep = PredefinedMenuItem::separator(&app).map_err(map_err)?;
    let studio = MenuItem::with_id(&app, "pet_studio", "打開桌寵工作室", true, None::<&str>)
        .map_err(map_err)?;

    let menu = Menu::with_items(&app, &[&hide, &switch, &size, &sep, &studio]).map_err(map_err)?;
    window.popup_menu(&menu).map_err(map_err)?;
    Ok(())
}

/// Route a menu-item click to a pet action. Returns `true` if `id` was a pet
/// menu id (so `main.rs` can stop routing). Non-pet ids (tray) return `false`.
pub fn handle_pet_menu_event(app: &AppHandle, id: &str) -> bool {
    match id {
        "pet_hide" => {
            if let Some(win) = app.get_webview_window(mascot_window::MASCOT_LABEL) {
                let _ = win.hide();
            }
            true
        }
        "pet_studio" => {
            pet_open_studio(app.clone());
            true
        }
        _ if id.starts_with("pet_size:") => {
            let scale = &id["pet_size:".len()..];
            let _ = pet_set_scale(app.clone(), scale.to_string());
            true
        }
        _ if id.starts_with("pet_switch:") => {
            let slug = id["pet_switch:".len()..].to_string();
            let _ = pet_activate(app.clone(), Some(slug));
            true
        }
        _ => false,
    }
}
