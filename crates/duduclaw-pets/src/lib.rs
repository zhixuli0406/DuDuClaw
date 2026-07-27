//! `duduclaw-pets` — photo → interactive desktop-pet packs (WP-P2/P3).
//!
//! Owns the pet pack **format** (`pet.json`, a Codex Pets superset), **disk
//! layout** (`~/.duduclaw/pets/<slug>/`), **slug sanitization**, and **local
//! background removal** (passthrough always; ONNX BiRefNet/silueta behind the
//! `onnx` feature). The desktop Tauri shell wraps these in commands; nothing here
//! depends on Tauri so the logic is unit-testable in the plain workspace.
//!
//! The P0 "original photo" flow: decode a photo → remove background → RGBA
//! cutout → save a `procedural`-mode pack → the runtime animates the single
//! image with spring physics.

pub mod error;
pub mod manifest;
pub mod pack;
pub mod pixelate;
pub mod segmentation;
pub mod slug;
pub mod sprite_bake;

pub use error::{PetError, Result};
pub use manifest::{
    default_procedural_behaviors, AnimationSpec, BehaviorSpec, PetManifest, PetMode, SCHEMA_VERSION,
};
pub use pack::{
    delete_pack, get_active_slug, list_packs, load_active, load_pack, pack_dir, pets_dir,
    save_procedural_pack, save_sprite_pack, set_active_slug, slug_exists, write_manifest, PetPack,
};
pub use pixelate::{pixelate_rgba, quantize_pixel_art, DEFAULT_PALETTE_SIZE, DEFAULT_PIXEL_WIDTH};
pub use segmentation::{
    models_dir, sha256_hex, BackgroundRemover, ModelSpec, ModelVariant, PassthroughRemover,
    BIREFNET_LITE, SILUETA,
};
pub use sprite_bake::{bake_spritesheet, spritesheet_animations, ROWS as SPRITE_ROWS};

#[cfg(feature = "onnx")]
pub use segmentation::{download_model, OnnxRemover};

/// Turn a sanitized base name into a unique, unused slug by appending `-2`,
/// `-3`, … when a pack of that name already exists.
pub fn allocate_slug(display_name: &str) -> String {
    let base = slug::sanitize_slug(display_name);
    if !pack::slug_exists(&base) {
        return base;
    }
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        if !pack::slug_exists(&candidate) {
            return candidate;
        }
    }
    // Astronomically unlikely; fall back to a timestamp suffix.
    format!("{base}-{}", chrono::Utc::now().timestamp())
}

/// End-to-end: remove the background from `photo_bytes` with `remover`, then
/// persist a procedural pet pack named `display_name`. Returns the saved pack.
///
/// This is the single orchestration entry the Tauri command calls. The original
/// photo is retained verbatim (`source.png`) for future re-generation.
pub fn generate_procedural_pet(
    display_name: &str,
    photo_bytes: &[u8],
    remover: &dyn BackgroundRemover,
) -> Result<PetPack> {
    if slug::sanitize_slug(display_name) == "pet" && display_name.trim().is_empty() {
        return Err(PetError::InvalidName(display_name.to_string()));
    }
    let cutout = remover.remove_background(photo_bytes)?;
    let unique = allocate_slug(display_name);
    pack::save_procedural_pack(&unique, display_name.trim(), &cutout, photo_bytes)
}

/// End-to-end pixel-art path: remove the background, quantise the cutout to a
/// retro pixel sprite, bake an 8×9 Codex Pets spritesheet from deterministic
/// action transforms, and persist a `sprite`-mode pack. Zero external API.
///
/// The original photo is retained verbatim for regeneration. Returns the pack.
pub fn generate_pixel_sprite_pet(
    display_name: &str,
    photo_bytes: &[u8],
    remover: &dyn BackgroundRemover,
) -> Result<PetPack> {
    if slug::sanitize_slug(display_name) == "pet" && display_name.trim().is_empty() {
        return Err(PetError::InvalidName(display_name.to_string()));
    }
    // 1. Background removal → RGBA cutout.
    let cutout_png = remover.remove_background(photo_bytes)?;
    let cutout = image::load_from_memory(&cutout_png)
        .map_err(|e| PetError::Image(format!("decode cutout failed: {e}")))?
        .to_rgba8();
    // 2. Pixel-art quantise (canonical low-res sprite).
    let pixel = pixelate::quantize_pixel_art(
        &cutout,
        pixelate::DEFAULT_PIXEL_WIDTH,
        pixelate::DEFAULT_PALETTE_SIZE,
    );
    // 3. Bake the action spritesheet.
    let sheet = sprite_bake::bake_spritesheet(&pixel);
    let sheet_png = segmentation::encode_png_rgba(&sheet)?;
    let animations = sprite_bake::spritesheet_animations();
    // 4. Persist a sprite-mode pack.
    let unique = allocate_slug(display_name);
    pack::save_sprite_pack(
        &unique,
        display_name.trim(),
        &sheet_png,
        photo_bytes,
        animations,
    )
}

/// Shared test helpers. `DUDUCLAW_HOME` is process-global, so **every** test that
/// touches the pets dir must go through [`testutil::with_temp_home`] — a single
/// crate-wide lock serializes them (per-module locks would race).
#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::Mutex;

    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with `DUDUCLAW_HOME` pointed at a throwaway temp dir, serialized
    /// crate-wide. Poison-tolerant (a prior panicking test doesn't cascade).
    pub fn with_temp_home<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        // SAFETY: serialized via ENV_LOCK; no other threads read env concurrently.
        unsafe { std::env::set_var("DUDUCLAW_HOME", tmp.path()) };
        f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::with_temp_home;

    fn tiny_png() -> Vec<u8> {
        let mut img = image::RgbaImage::new(4, 4);
        for p in img.pixels_mut() {
            *p = image::Rgba([120, 130, 140, 255]);
        }
        segmentation::encode_png_rgba(&img).unwrap()
    }

    #[test]
    fn generate_via_passthrough_end_to_end() {
        with_temp_home(|| {
            let photo = tiny_png();
            let pack = generate_procedural_pet("嘟嘟", &photo, &PassthroughRemover).unwrap();
            assert_eq!(pack.slug, "嘟嘟");
            assert_eq!(pack.manifest.mode, PetMode::Procedural);
            assert!(pack.dir.join("sprite.png").is_file());
            // Re-generating with the same name allocates a distinct slug.
            let pack2 = generate_procedural_pet("嘟嘟", &photo, &PassthroughRemover).unwrap();
            assert_eq!(pack2.slug, "嘟嘟-2");
        });
    }

    #[test]
    fn generate_pixel_sprite_end_to_end() {
        with_temp_home(|| {
            let photo = tiny_png();
            let pack = generate_pixel_sprite_pet("像素嘟", &photo, &PassthroughRemover).unwrap();
            assert_eq!(pack.manifest.mode, PetMode::Sprite);
            // Sprite pack writes the baked grid + retains the source photo.
            assert!(pack.dir.join("spritesheet.png").is_file());
            assert!(pack.dir.join("source.png").is_file());
            assert_eq!(
                pack.manifest.spritesheet_path.as_deref(),
                Some("spritesheet.png")
            );
            // All 9 animation rows made it into the manifest.
            assert_eq!(pack.manifest.animations.len(), SPRITE_ROWS.len());
            assert!(pack.manifest.animations.contains_key("idle"));

            // pet.json round-trips: reload sees sprite mode + the spritesheet.
            let loaded = load_pack(&pack.slug).unwrap();
            assert_eq!(loaded.manifest.mode, PetMode::Sprite);
            assert!(loaded.image_path().unwrap().ends_with("spritesheet.png"));
            assert_eq!(loaded.manifest.animations.len(), SPRITE_ROWS.len());
        });
    }

    #[test]
    fn allocate_slug_resolves_collisions() {
        with_temp_home(|| {
            assert_eq!(allocate_slug("My Pet"), "my-pet");
            pack::save_procedural_pack("my-pet", "My Pet", &tiny_png(), b"x").unwrap();
            assert_eq!(allocate_slug("My Pet"), "my-pet-2");
        });
    }
}
