//! On-disk pet packs under `~/.duduclaw/pets/`.
//!
//! Layout:
//! ```text
//! ~/.duduclaw/pets/
//!   active.json          # { "slug": "<active pet slug>" }
//!   <slug>/
//!     pet.json           # manifest
//!     sprite.png         # procedural cutout (RGBA)   OR  spritesheet.webp
//!     source.png         # retained original photo
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PetError, Result};
use crate::manifest::PetManifest;

/// The pets root (`~/.duduclaw/pets/`).
pub fn pets_dir() -> PathBuf {
    duduclaw_core::duduclaw_home().join("pets")
}

/// The directory for a single pet pack.
pub fn pack_dir(slug: &str) -> PathBuf {
    pets_dir().join(slug)
}

/// A loaded pet pack: its slug, manifest, and absolute pack directory.
#[derive(Debug, Clone)]
pub struct PetPack {
    pub slug: String,
    pub manifest: PetManifest,
    pub dir: PathBuf,
}

impl PetPack {
    /// Absolute path to the renderable image (procedural sprite, else spritesheet).
    pub fn image_path(&self) -> Option<PathBuf> {
        self.manifest
            .sprite
            .as_ref()
            .or(self.manifest.spritesheet_path.as_ref())
            .map(|rel| self.dir.join(rel))
    }
}

/// Pointer file naming the currently-active pet.
#[derive(Debug, Serialize, Deserialize, Default)]
struct ActivePointer {
    slug: Option<String>,
}

fn active_path() -> PathBuf {
    pets_dir().join("active.json")
}

/// Persist a freshly generated procedural pack to disk and return its slug.
///
/// `unique_slug` must already be sanitized + collision-resolved (see
/// [`crate::allocate_slug`]). `sprite_png` is RGBA PNG bytes; `source` is the
/// original photo bytes (stored verbatim as `source.png`).
pub fn save_procedural_pack(
    unique_slug: &str,
    display_name: &str,
    sprite_png: &[u8],
    source: &[u8],
) -> Result<PetPack> {
    let dir = pack_dir(unique_slug);
    std::fs::create_dir_all(&dir)?;

    std::fs::write(dir.join("sprite.png"), sprite_png)?;
    std::fs::write(dir.join("source.png"), source)?;

    let manifest =
        PetManifest::new_procedural(unique_slug, display_name, "sprite.png", "source.png");
    write_manifest(&dir, &manifest)?;

    Ok(PetPack {
        slug: unique_slug.to_string(),
        manifest,
        dir,
    })
}

/// Write `pet.json` into a pack directory (pretty JSON).
pub fn write_manifest(dir: &Path, manifest: &PetManifest) -> Result<()> {
    std::fs::write(dir.join("pet.json"), manifest.to_json()?)?;
    Ok(())
}

/// Load a single pack by slug.
pub fn load_pack(slug: &str) -> Result<PetPack> {
    let dir = pack_dir(slug);
    let manifest_path = dir.join("pet.json");
    if !manifest_path.is_file() {
        return Err(PetError::NotFound(slug.to_string()));
    }
    let bytes = std::fs::read(&manifest_path)?;
    let manifest = PetManifest::from_json(&bytes)?;
    Ok(PetPack {
        slug: slug.to_string(),
        manifest,
        dir,
    })
}

/// List every valid pack under the pets dir (each must have a readable `pet.json`).
/// Unreadable / malformed packs are skipped (never fail the whole listing).
pub fn list_packs() -> Vec<PetPack> {
    let root = pets_dir();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut packs = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        match load_pack(&slug) {
            Ok(p) => packs.push(p),
            Err(e) => tracing::debug!(slug, error = %e, "skipping unreadable pet pack"),
        }
    }
    packs.sort_by(|a, b| a.slug.cmp(&b.slug));
    packs
}

/// Whether a pack directory already exists for `slug`.
pub fn slug_exists(slug: &str) -> bool {
    pack_dir(slug).is_dir()
}

/// Delete a pet pack directory. No-op (Ok) if it does not exist. Clears the
/// active pointer if it named this pet.
pub fn delete_pack(slug: &str) -> Result<()> {
    let dir = pack_dir(slug);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    if get_active_slug().as_deref() == Some(slug) {
        set_active_slug(None)?;
    }
    Ok(())
}

/// Read the active pet slug, if any (and only if that pack still exists).
pub fn get_active_slug() -> Option<String> {
    let bytes = std::fs::read(active_path()).ok()?;
    let ptr: ActivePointer = serde_json::from_slice(&bytes).ok()?;
    ptr.slug.filter(|s| slug_exists(s))
}

/// Set (or, with `None`, clear) the active pet.
pub fn set_active_slug(slug: Option<&str>) -> Result<()> {
    let root = pets_dir();
    std::fs::create_dir_all(&root)?;
    if let Some(s) = slug.filter(|s| !slug_exists(s)) {
        return Err(PetError::NotFound(s.to_string()));
    }
    let ptr = ActivePointer {
        slug: slug.map(str::to_string),
    };
    let json = serde_json::to_vec_pretty(&ptr).map_err(|e| PetError::Manifest(e.to_string()))?;
    std::fs::write(active_path(), json)?;
    Ok(())
}

/// Load the active pack (if one is set and still valid).
pub fn load_active() -> Option<PetPack> {
    let slug = get_active_slug()?;
    load_pack(&slug).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::with_temp_home;

    fn tiny_png() -> Vec<u8> {
        let mut img = image::RgbaImage::new(3, 3);
        for p in img.pixels_mut() {
            *p = image::Rgba([10, 20, 30, 255]);
        }
        crate::segmentation::encode_png_rgba(&img).unwrap()
    }

    #[test]
    fn save_load_list_and_activate() {
        with_temp_home(|| {
            let png = tiny_png();
            let pack = save_procedural_pack("kuro", "小黑貓", &png, b"rawphoto").unwrap();
            assert_eq!(pack.slug, "kuro");
            assert!(pack.dir.join("sprite.png").is_file());
            assert!(pack.dir.join("source.png").is_file());
            assert!(pack.dir.join("pet.json").is_file());

            // Round-trip load.
            let loaded = load_pack("kuro").unwrap();
            assert_eq!(loaded.manifest.display_name, "小黑貓");
            assert!(loaded.image_path().unwrap().ends_with("sprite.png"));

            // Listing sees it.
            let all = list_packs();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].slug, "kuro");

            // Activation.
            assert!(get_active_slug().is_none());
            set_active_slug(Some("kuro")).unwrap();
            assert_eq!(get_active_slug().as_deref(), Some("kuro"));
            assert_eq!(load_active().unwrap().slug, "kuro");

            // Activating a missing pet fails closed.
            assert!(set_active_slug(Some("ghost")).is_err());
        });
    }

    #[test]
    fn delete_clears_active_pointer() {
        with_temp_home(|| {
            let png = tiny_png();
            save_procedural_pack("kuro", "K", &png, b"x").unwrap();
            set_active_slug(Some("kuro")).unwrap();
            delete_pack("kuro").unwrap();
            assert!(!slug_exists("kuro"));
            assert!(get_active_slug().is_none());
            // Deleting again is a no-op.
            delete_pack("kuro").unwrap();
        });
    }

    #[test]
    fn missing_pack_is_not_found() {
        with_temp_home(|| {
            assert!(matches!(load_pack("nope"), Err(PetError::NotFound(_))));
            assert!(list_packs().is_empty());
        });
    }
}
