# Photo → Interactive Desktop Pet

> Drop in one photo, get a pixel-art desktop companion that wanders your screen — generated entirely on your machine, no cloud image model.

---

## What It Is

The desktop app (v1.46) turns a photo — your cat, your kid's drawing, a plush toy — into an animated always-on-top desktop pet. The whole pipeline is local: background removal, pixel-art conversion, and spritesheet baking all run inside the `duduclaw-pets` crate with zero external API calls. The pet then lives in a small transparent window, plays idle animations, wanders across the desktop on its own, and reacts to agent activity.

## Generation Pipeline

```text
photo → EXIF orientation fix → background removal → RGBA cutout
      → pixel quantization → 8×9 spritesheet bake → pet pack on disk
```

1. **Background removal** (`segmentation.rs`): local ONNX inference with BiRefNet-general-lite (primary) or silueta (low-resource fallback), behind the `onnx` feature. A `PassthroughRemover` is always available — it backs the "I already cut out the background" flow and guarantees generation works with no model installed. Models live in `~/.duduclaw/models/` (shared with the GGUF store) and are downloaded on demand, never at build time. Phone photos get their EXIF `Orientation` tag applied so portraits don't arrive sideways.
2. **Pixel quantization** (`pixelate.rs`): nearest-neighbour downscale to a 64-pixel-wide canonical grid, hard alpha threshold (pixel art has hard edges), then median-cut color quantization to a 16-color palette — so the result reads as a limited-palette sprite, not a shrunk photo.
3. **Spritesheet bake** (`sprite_bake.rs`): the single sprite is turned into an 8-column × 9-row grid of animation frames using only deterministic geometric transforms (scale / translate / flip / shear) — no drawing model. Rows follow the openpets / Codex Pets layout (idle, running-right, running-left, waving, jumping, failed, waiting, running, review) with 192×208 frame cells, so a baked sheet drops straight into that ecosystem.

Packs land in `~/.duduclaw/pets/<slug>/` with a `pet.json` manifest (a Codex Pets superset). The original photo is kept verbatim (`source.png`) so a pet can be regenerated later without re-uploading.

## Two Pet Modes

| Mode | Source | Animation |
|---|---|---|
| `procedural` | one background-removed cutout | WAAPI keyframes + a hand-rolled spring on the single image |
| `sprite` | baked 8×9 spritesheet | per-state frame stepping on a `<canvas>` with nearest-neighbour scaling |

The procedural path is the P0 flow (fast, works on any photo); the sprite path is the pixel-art flow. Both are driven by the same runtime and the same interaction state machine.

## The Window

`mascot_window.rs` builds a second Tauri window: transparent, borderless, always-on-top, skipping the taskbar, 180 logical px base size, hidden until the tray toggles it. On macOS the window's own drop shadow is disabled — a transparent decorationless window otherwise still draws a rectangular shadow outlining its bounds, which is the "box around the pet" users reported. Only the pet's own pixels remain visible.

## The Wander Engine

While idle, `PetRuntime.tsx` periodically picks a weighted-random behavior:

- **Walk left / right** — moves the *real* window across the desktop via `petMoveBy`, bouncing to the opposite direction at screen edges.
- **Rest** (sit down), **wave**, **jump** — timed in-place animations.

Any interaction or agent signal interrupts the wander instantly: every behavior re-checks the live state before acting, so a press or drag takes over mid-walk. All autonomous motion is gated on `prefers-reduced-motion`, checked at pick time so a live setting change takes effect immediately. Outside Tauri (plain browser), walks degrade to in-place animation without window movement.

## Interactions

- **Drag** — a gesture (mousedown + move past 4 px starts a native window drag), deliberately not `data-tauri-drag-region`, so a plain click still registers as a click (Tauri #9751/#9901). Release triggers a fall animation.
- **Click** — a reaction animation; 60 s without interaction and the pet dozes off.
- **Right-click** — native context menu, including "open studio" (navigates the main window to the pet studio) and size selection.
- **Scale** — small / standard / large (50% / 100% / 150% of the 180 px base). The host resizes the *window*; the pet tracks the viewport so scaling never grows an invisible frame.

## Agent Signals

The pet doubles as a status surface. An agent signal (`working` / `notify` / `idle`) switches its state: `working` plays the busy animation, `notify` raises a placard the user can dismiss (it re-arms when new items arrive). Pending approvals are the first live wiring — outstanding approval requests raise the notify placard; a live agent-status feed for `working` is the planned next hook.

## Pet Studio

The dashboard's pet studio lists all generated packs (name, mode, active flag), lets you generate from a new photo, pick the active pet, and delete packs. Studio and overlay talk to the same thin Tauri command layer (`pet_gen.rs`); a `pet://changed` event broadcasts to all windows when the active pet switches, so the overlay re-fetches without a restart.

## Limits

| Aspect | Value |
|---|---|
| Pixel grid width | 64 px (canonical) |
| Palette | 16 colors (median-cut) |
| Spritesheet | 8 cols × 9 rows, 192×208 px cells |
| Cloud calls during generation | none |
| Background-removal models | BiRefNet-lite / silueta (optional), passthrough always |
