//! `pet.json` manifest — a superset of the Codex Pets format.
//!
//! A bare Codex Pets pack (only `id` / `displayName` / `description` /
//! `spritesheetPath`) deserializes cleanly because every DuDuClaw-only field has
//! a default. DuDuClaw adds:
//! - `mode` — `sprite` (grid spritesheet, Codex Pets style) or `procedural`
//!   (single background-removed image + code-driven spring animation — the P0
//!   "original photo" path where one image is enough to animate).
//! - `sprite` / `source` — procedural cutout + retained original photo.
//! - `animations` — per-state grid animation specs (Shimeji-style pose+velocity).
//! - `behaviors` — weighted-random / conditional state selection table.
//!
//! Everything is camelCase on the wire so petdex / openpets assets drop in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Current DuDuClaw pet schema version. Bumped on breaking manifest changes.
pub const SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// How a pet is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PetMode {
    /// Grid spritesheet, one row per state (Codex Pets / openpets). The default
    /// so a manifest that only carries `spritesheetPath` behaves like Codex Pets.
    #[default]
    Sprite,
    /// Single background-removed image + procedural (spring-physics) animation.
    /// The P0 "original photo" path — one PNG is enough to bring a pet to life.
    Procedural,
}

/// One grid animation (sprite mode). Borrows Shimeji's pose+velocity idea and
/// VPet's optional `start`/`end` bookend frames for smooth enter/exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationSpec {
    /// Zero-based row in the spritesheet grid.
    pub row: u32,
    /// Number of frames in the row to play.
    pub frames: u32,
    /// Playback frames-per-second.
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Whether the animation loops (vs. plays once).
    #[serde(default = "default_true", rename = "loop")]
    pub loops: bool,
    /// Optional per-frame desktop velocity `[dx, dy]` in px (Shimeji walk/climb).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<[f32; 2]>,
}

fn default_fps() -> u32 {
    8
}
fn default_true() -> bool {
    true
}

/// A weighted / conditional behavior entry: pick `state` with weight `frequency`,
/// optionally gated by a named `condition` (e.g. `at_edge`, `has_pending_task`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSpec {
    /// State to enter (must exist in `animations` for sprite mode, or be one of
    /// the built-in procedural states: idle/drag/fall/click/working/notify/sleep).
    pub state: String,
    /// Relative selection weight among currently-eligible behaviors.
    pub frequency: f32,
    /// Optional named precondition; `None` = always eligible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// The `pet.json` manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetManifest {
    /// DuDuClaw schema version (defaults to current for bare Codex Pets packs).
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Stable identifier (Codex Pets field). Usually equals the folder slug.
    pub id: String,
    /// Human-facing name shown in the studio + tray (Codex Pets field).
    pub display_name: String,
    /// Short description (Codex Pets field).
    #[serde(default)]
    pub description: String,
    /// Grid spritesheet path relative to the pack dir (Codex Pets field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spritesheet_path: Option<String>,
    /// Render mode. Defaults to `sprite` for Codex Pets compatibility.
    #[serde(default)]
    pub mode: PetMode,
    /// Procedural single-image cutout path relative to the pack dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    /// Retained original photo path relative to the pack dir (for regeneration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Per-state grid animations (sprite mode). Empty for pure procedural packs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub animations: BTreeMap<String, AnimationSpec>,
    /// Weighted / conditional behavior table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<BehaviorSpec>,
    /// RFC3339 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl PetManifest {
    /// Build a fresh procedural pet manifest (the P0 "original photo" path).
    ///
    /// `sprite_file` / `source_file` are pack-relative filenames. A default
    /// behavior table seeds the idle-variant weighting the runtime reads.
    pub fn new_procedural(
        id: impl Into<String>,
        display_name: impl Into<String>,
        sprite_file: impl Into<String>,
        source_file: impl Into<String>,
    ) -> Self {
        PetManifest {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            display_name: display_name.into(),
            description: String::new(),
            spritesheet_path: None,
            mode: PetMode::Procedural,
            sprite: Some(sprite_file.into()),
            source: Some(source_file.into()),
            animations: BTreeMap::new(),
            behaviors: default_procedural_behaviors(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    /// Build a fresh sprite pet manifest (baked pixel-art spritesheet path).
    ///
    /// `spritesheet_file` / `source_file` are pack-relative filenames;
    /// `animations` is the per-state grid table (see `sprite_bake`).
    pub fn new_sprite(
        id: impl Into<String>,
        display_name: impl Into<String>,
        spritesheet_file: impl Into<String>,
        source_file: impl Into<String>,
        animations: BTreeMap<String, AnimationSpec>,
    ) -> Self {
        PetManifest {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            display_name: display_name.into(),
            description: String::new(),
            spritesheet_path: Some(spritesheet_file.into()),
            mode: PetMode::Sprite,
            sprite: None,
            source: Some(source_file.into()),
            animations,
            behaviors: default_procedural_behaviors(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    /// Parse a manifest from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> crate::Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| crate::PetError::Manifest(e.to_string()))
    }

    /// Serialize to pretty JSON bytes.
    pub fn to_json(&self) -> crate::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|e| crate::PetError::Manifest(e.to_string()))
    }
}

/// Default weighted idle-variant behaviors for a procedural pet. The runtime
/// treats these as hints for the weighted-random idle sub-states; the physics
/// (drag/fall/click) are hard-chained in code and always available.
pub fn default_procedural_behaviors() -> Vec<BehaviorSpec> {
    vec![
        BehaviorSpec {
            state: "idle".into(),
            frequency: 8.0,
            condition: None,
        },
        BehaviorSpec {
            state: "click".into(),
            frequency: 1.0,
            condition: Some("on_click".into()),
        },
        BehaviorSpec {
            state: "sleep".into(),
            frequency: 1.0,
            condition: Some("idle_timeout".into()),
        },
        BehaviorSpec {
            state: "notify".into(),
            frequency: 1.0,
            condition: Some("has_pending_task".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procedural_roundtrip() {
        let m = PetManifest::new_procedural("kuro", "小黑貓", "sprite.png", "source.png");
        let bytes = m.to_json().unwrap();
        let back = PetManifest::from_json(&bytes).unwrap();
        assert_eq!(back.id, "kuro");
        assert_eq!(back.display_name, "小黑貓");
        assert_eq!(back.mode, PetMode::Procedural);
        assert_eq!(back.sprite.as_deref(), Some("sprite.png"));
        assert!(!back.behaviors.is_empty());
    }

    #[test]
    fn bare_codex_pets_pack_loads() {
        // A minimal Codex Pets manifest with none of the DuDuClaw fields.
        let json = br#"{
            "id": "fox",
            "displayName": "Foxy",
            "description": "a classic sprite pet",
            "spritesheetPath": "spritesheet.webp"
        }"#;
        let m = PetManifest::from_json(json).unwrap();
        assert_eq!(m.id, "fox");
        assert_eq!(m.display_name, "Foxy");
        // Defaults kick in: sprite mode, current schema version, empty tables.
        assert_eq!(m.mode, PetMode::Sprite);
        assert_eq!(m.schema_version, SCHEMA_VERSION);
        assert_eq!(m.spritesheet_path.as_deref(), Some("spritesheet.webp"));
        assert!(m.animations.is_empty());
        assert!(m.behaviors.is_empty());
        assert!(m.sprite.is_none());
    }

    #[test]
    fn animation_loop_keyword_serializes_as_loop() {
        let mut anims = BTreeMap::new();
        anims.insert(
            "walk".to_string(),
            AnimationSpec {
                row: 2,
                frames: 8,
                fps: 12,
                loops: true,
                velocity: Some([2.0, 0.0]),
            },
        );
        let m = PetManifest {
            schema_version: SCHEMA_VERSION,
            id: "w".into(),
            display_name: "W".into(),
            description: String::new(),
            spritesheet_path: Some("s.webp".into()),
            mode: PetMode::Sprite,
            sprite: None,
            source: None,
            animations: anims,
            behaviors: vec![],
            created_at: None,
        };
        let json = String::from_utf8(m.to_json().unwrap()).unwrap();
        // The Rust field `loops` must serialize as JSON key "loop".
        assert!(json.contains("\"loop\""));
        assert!(!json.contains("\"loops\""));
        // Round-trips back.
        let back = PetManifest::from_json(json.as_bytes()).unwrap();
        assert!(back.animations["walk"].loops);
        assert_eq!(back.animations["walk"].velocity, Some([2.0, 0.0]));
    }

    #[test]
    fn camel_case_on_wire() {
        let m = PetManifest::new_procedural("k", "K", "sprite.png", "source.png");
        let json = String::from_utf8(m.to_json().unwrap()).unwrap();
        assert!(json.contains("\"displayName\""));
        assert!(json.contains("\"schemaVersion\""));
        assert!(json.contains("\"createdAt\""));
        assert!(!json.contains("\"display_name\""));
    }
}
