/**
 * Isometric world geometry for the PixiJS 2D world stage.
 *
 * The scenes are authored as tile matrices (`matrix[y][x]`); this module maps a
 * tile grid onto a classic 2:1 isometric projection — a tile is twice as wide
 * as it is tall — and owns the *pure* camera math (contain-fit framing, the
 * zoom clamp + anchor maths, the pan clamp, and the frame-rate-independent
 * interpolation helpers). No PixiJS, no DOM, no globals: this is the tested
 * half; the renderer (`stage-scene.ts`) consumes these numbers.
 *
 * A tile `(gridX, gridY)` at elevation `level` projects to the *screen* with the
 * diamond's top corner at {@link tileToScreen}; the diamond spans one tile-width
 * east/west and one tile-height south. Elevation lifts a tile straight up the
 * screen. Painter's-order depth ({@link depthAt}) collapses the world to one
 * scalar derived from `gridX + gridY + level`, exactly the sort key the actors
 * layer needs each frame.
 */

/** Screen width of one tile diamond (2:1 iso — width is twice the height). */
export const TILE_WIDTH = 64;
/** Screen height of one tile diamond. */
export const TILE_HEIGHT = 32;
export const HALF_TILE_WIDTH = TILE_WIDTH / 2;
export const HALF_TILE_HEIGHT = TILE_HEIGHT / 2;

/** Screen pixels a single elevation level lifts a tile upward. */
export const ELEVATION_HEIGHT = 26;

export interface Point {
  readonly x: number;
  readonly y: number;
}

// ---- Projection ------------------------------------------------------------

/**
 * Project a tile's *top corner* into local (unscaled) screen space. `gridX` runs
 * down-right, `gridY` runs down-left; elevation lifts straight up. Deterministic,
 * so the ground, furniture and characters all share one coordinate frame.
 */
export function tileToScreen(gridX: number, gridY: number, level = 0): Point {
  return {
    x: (gridX - gridY) * HALF_TILE_WIDTH,
    y: (gridX + gridY) * HALF_TILE_HEIGHT - level * ELEVATION_HEIGHT,
  };
}

/** Project the *centre* of a tile (where an agent's feet rest). */
export function tileCenterToScreen(gridX: number, gridY: number, level = 0): Point {
  const corner = tileToScreen(gridX, gridY, level);
  return { x: corner.x, y: corner.y + HALF_TILE_HEIGHT };
}

/**
 * Inverse projection: turn a local screen position back into fractional tile
 * coordinates. Elevation is ignored (callers resolve height from the grid). Used
 * to keep a wheel-zoom anchored under the cursor.
 */
export function screenToTile(screenX: number, screenY: number): Point {
  const nx = screenX / HALF_TILE_WIDTH;
  const ny = screenY / HALF_TILE_HEIGHT;
  return { x: (ny + nx) / 2, y: (ny - nx) / 2 };
}

// ---- Depth sorting ---------------------------------------------------------

/** Depth scale per tile step — leaves headroom for per-entity layer biases. */
export const DEPTH_TILE_SCALE = 16;
export const LAYER_FLOOR = 0;
export const LAYER_DECAL = 1;
export const LAYER_WALL = 2;
export const LAYER_FURNITURE = 3;
export const LAYER_AGENT = 6;

/** Painter's-order depth for a point at `(gridX, gridY)` and elevation `level`. */
export function depthAt(gridX: number, gridY: number, level = 0, layer = 0): number {
  return (gridX + gridY) * DEPTH_TILE_SCALE + level * (DEPTH_TILE_SCALE / 2) + layer;
}

// ---- Scene bounds ----------------------------------------------------------

export interface Bounds {
  readonly minX: number;
  readonly minY: number;
  readonly width: number;
  readonly height: number;
}

/**
 * Local-space bounding box of a `cols × rows` iso grid, accounting for the
 * diamond extents (±half-width, +tile-height) and the tallest elevation. Feeds
 * {@link containFit} so the camera frames the whole scene.
 */
export function isoGridBounds(cols: number, rows: number, maxLevel = 0): Bounds {
  // Diamond corners over the grid extremes.
  const leftX = tileToScreen(0, rows - 1).x - HALF_TILE_WIDTH;
  const rightX = tileToScreen(cols - 1, 0).x + HALF_TILE_WIDTH;
  const topY = tileToScreen(0, 0, maxLevel).y - HALF_TILE_HEIGHT;
  const bottomY = tileToScreen(cols - 1, rows - 1).y + TILE_HEIGHT;
  return { minX: leftX, minY: topY, width: rightX - leftX, height: bottomY - topY };
}

// ---- Camera math (pure) ----------------------------------------------------

/** Minimum / maximum camera zoom (world scale). */
export const MIN_ZOOM = 0.5;
export const MAX_ZOOM = 2.5;

/** Clamp a zoom scale into `[min, max]`. */
export function clampZoom(scale: number, min: number = MIN_ZOOM, max: number = MAX_ZOOM): number {
  return Math.min(max, Math.max(min, scale));
}

/**
 * Fit `content` inside a `viewW × viewH` viewport with a uniform scale (never
 * upscaling past 1× unless the content is tiny), returning the world-container
 * `scale` and `position` (screen offset) that centres the content. Pure.
 *
 * A world point `wp` maps to screen as `wp * scale + position`, so to centre the
 * content box we solve for the position that lands its centre at the viewport
 * centre.
 */
export function containFit(
  content: Bounds,
  viewW: number,
  viewH: number,
  padding = 24,
  clamp: { min: number; max: number } = { min: MIN_ZOOM, max: MAX_ZOOM },
): { scale: number; x: number; y: number } {
  const availW = Math.max(1, viewW - padding * 2);
  const availH = Math.max(1, viewH - padding * 2);
  const raw = content.width > 0 && content.height > 0
    ? Math.min(availW / content.width, availH / content.height)
    : 1;
  const scale = clampZoom(raw, clamp.min, clamp.max);
  const contentCx = content.minX + content.width / 2;
  const contentCy = content.minY + content.height / 2;
  return {
    scale,
    x: viewW / 2 - contentCx * scale,
    y: viewH / 2 - contentCy * scale,
  };
}

/**
 * New camera position after zooming to `newScale` while keeping the world point
 * currently under the screen anchor `(anchorX, anchorY)` fixed. Pure — returns a
 * fresh point. `pos`/`scale` are the pre-zoom camera position/scale.
 */
export function zoomAtPoint(
  pos: Point,
  scale: number,
  newScale: number,
  anchorX: number,
  anchorY: number,
): Point {
  // world_under_anchor = (anchor - pos) / scale ; keep it fixed after rescale.
  const k = newScale / scale;
  return {
    x: anchorX - (anchorX - pos.x) * k,
    y: anchorY - (anchorY - pos.y) * k,
  };
}

/**
 * Clamp a camera position so the scaled content can't be dragged entirely out of
 * view: the content's scaled box must always overlap the viewport by at least
 * `margin` px on every side. Pure — returns a fresh point.
 */
export function clampPan(
  pos: Point,
  content: Bounds,
  scale: number,
  viewW: number,
  viewH: number,
  margin = 80,
): Point {
  const w = content.width * scale;
  const h = content.height * scale;
  // Screen-space extent of the content given `pos`.
  const left = pos.x + content.minX * scale;
  const top = pos.y + content.minY * scale;
  // Allowed range for the content's left/top edges.
  const minLeft = Math.min(margin, viewW - w - margin);
  const maxLeft = Math.max(viewW - w - margin, margin);
  const minTop = Math.min(margin, viewH - h - margin);
  const maxTop = Math.max(viewH - h - margin, margin);
  const clampedLeft = Math.min(maxLeft, Math.max(minLeft, left));
  const clampedTop = Math.min(maxTop, Math.max(minTop, top));
  return {
    x: pos.x + (clampedLeft - left),
    y: pos.y + (clampedTop - top),
  };
}

// ---- Interpolation (renderer animation helpers) ----------------------------

/** Linear interpolation between two scalars. */
export function lerp(from: number, to: number, amount: number): number {
  return from + (to - from) * amount;
}

/**
 * Frame-rate-independent smoothing factor for a lerp toward a target. `rate` is
 * the fraction closed per (1/60 s) frame; `dtSeconds` is the frame delta. Returns
 * an alpha in [0, 1] to feed {@link lerp}.
 */
export function smoothAlpha(rate: number, dtSeconds: number): number {
  if (dtSeconds <= 0) return 0;
  const a = 1 - Math.pow(1 - Math.min(1, Math.max(0, rate)), dtSeconds * 60);
  return Math.min(1, Math.max(0, a));
}
