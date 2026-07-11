import { describe, it, expect } from 'vitest';
import {
  TILE_WIDTH,
  TILE_HEIGHT,
  HALF_TILE_WIDTH,
  HALF_TILE_HEIGHT,
  ELEVATION_HEIGHT,
  tileToScreen,
  tileCenterToScreen,
  screenToTile,
  depthAt,
  isoGridBounds,
  containFit,
  zoomAtPoint,
  clampPan,
  clampZoom,
  lerp,
  smoothAlpha,
  MIN_ZOOM,
  MAX_ZOOM,
  LAYER_AGENT,
} from './geometry';

describe('tile → iso screen projection', () => {
  it('origin tile projects to the screen origin', () => {
    const p = tileToScreen(0, 0);
    expect(p.x).toBe(0);
    expect(p.y).toBe(0);
  });

  it('+gridX moves down-right, +gridY moves down-left (2:1 iso)', () => {
    const a = tileToScreen(1, 0);
    expect(a.x).toBe(HALF_TILE_WIDTH);
    expect(a.y).toBe(HALF_TILE_HEIGHT);
    const b = tileToScreen(0, 1);
    expect(b.x).toBe(-HALF_TILE_WIDTH);
    expect(b.y).toBe(HALF_TILE_HEIGHT);
  });

  it('elevation lifts a tile straight up by ELEVATION_HEIGHT per level', () => {
    const flat = tileToScreen(2, 2, 0);
    const up = tileToScreen(2, 2, 2);
    expect(up.x).toBe(flat.x);
    expect(flat.y - up.y).toBe(2 * ELEVATION_HEIGHT);
  });

  it('tile centre sits half a tile-height below the top corner', () => {
    const centre = tileCenterToScreen(3, 1);
    const corner = tileToScreen(3, 1);
    expect(centre.y - corner.y).toBe(HALF_TILE_HEIGHT);
  });

  it('screenToTile inverts tileToScreen (round-trip)', () => {
    for (const [gx, gy] of [[0, 0], [3, 5], [7, 2]] as const) {
      const s = tileToScreen(gx, gy);
      const t = screenToTile(s.x, s.y);
      expect(t.x).toBeCloseTo(gx, 6);
      expect(t.y).toBeCloseTo(gy, 6);
    }
  });

  it('tiles are 2:1 (width twice height)', () => {
    expect(TILE_WIDTH).toBe(TILE_HEIGHT * 2);
  });
});

describe('depth sorting', () => {
  it('a tile further down-screen sorts in front (larger depth)', () => {
    expect(depthAt(2, 2)).toBeGreaterThan(depthAt(1, 1));
    expect(depthAt(0, 3)).toBe(depthAt(3, 0)); // same gridX+gridY row
  });

  it('a layer bias breaks ties on the same tile without crossing tiles', () => {
    const floor = depthAt(2, 2, 0, 0);
    const agent = depthAt(2, 2, 0, LAYER_AGENT);
    expect(agent).toBeGreaterThan(floor);
    // The agent bias must not push it past the next tile down.
    expect(agent).toBeLessThan(depthAt(3, 2, 0, 0));
  });
});

describe('scene bounds', () => {
  it('a bigger grid gets a bigger bounding box', () => {
    const small = isoGridBounds(5, 5);
    const big = isoGridBounds(20, 20);
    expect(big.width).toBeGreaterThan(small.width);
    expect(big.height).toBeGreaterThan(small.height);
  });

  it('elevation extends the box upward (smaller minY)', () => {
    const flat = isoGridBounds(10, 10, 0);
    const tall = isoGridBounds(10, 10, 3);
    expect(tall.minY).toBeLessThan(flat.minY);
    expect(tall.height).toBeGreaterThan(flat.height);
  });
});

describe('containFit (frame the whole scene)', () => {
  it('centres content in the viewport', () => {
    const content = { minX: -100, minY: -50, width: 200, height: 100 };
    const fit = containFit(content, 800, 600, 0);
    // Content centre is (0, 0) → lands at the viewport centre.
    expect(fit.x).toBeCloseTo(400, 6);
    expect(fit.y).toBeCloseTo(300, 6);
  });

  it('picks the limiting axis and clamps into the zoom range', () => {
    // Very wide content → width-limited; padding respected.
    const wide = { minX: 0, minY: 0, width: 4000, height: 100 };
    const fit = containFit(wide, 800, 600, 20);
    expect(fit.scale).toBeGreaterThanOrEqual(MIN_ZOOM);
    expect(fit.scale).toBeLessThanOrEqual(MAX_ZOOM);
    // (800 - 40) / 4000 = 0.19 → clamped up to MIN_ZOOM.
    expect(fit.scale).toBe(MIN_ZOOM);
  });
});

describe('camera zoom', () => {
  it('clampZoom bounds the scale', () => {
    expect(clampZoom(1)).toBe(1);
    expect(clampZoom(0.1)).toBe(MIN_ZOOM);
    expect(clampZoom(9)).toBe(MAX_ZOOM);
  });

  it('zoomAtPoint keeps the world point under the anchor fixed', () => {
    const pos = { x: 100, y: 50 };
    const scale = 1;
    const anchorX = 300;
    const anchorY = 200;
    const worldUnder = { x: (anchorX - pos.x) / scale, y: (anchorY - pos.y) / scale };
    const next = zoomAtPoint(pos, scale, 2, anchorX, anchorY);
    // The same world point must still project to the anchor after the rescale.
    expect(next.x + worldUnder.x * 2).toBeCloseTo(anchorX, 6);
    expect(next.y + worldUnder.y * 2).toBeCloseTo(anchorY, 6);
  });
});

describe('camera pan clamp', () => {
  const content = { minX: -200, minY: -100, width: 400, height: 200 };

  it('a centred position is left untouched when the content overlaps the view', () => {
    const centred = containFit(content, 800, 600, 0);
    const clamped = clampPan({ x: centred.x, y: centred.y }, content, centred.scale, 800, 600, 80);
    expect(clamped.x).toBeCloseTo(centred.x, 6);
    expect(clamped.y).toBeCloseTo(centred.y, 6);
  });

  it('pulls a runaway position back so the content stays partly visible', () => {
    // Drag the world far off to the right → clamp reels it back.
    const clamped = clampPan({ x: 99999, y: 0 }, content, 1, 800, 600, 80);
    // The content's left edge may sit at most at (viewW - margin).
    const left = clamped.x + content.minX * 1;
    expect(left).toBeLessThanOrEqual(800 - 80 + 1e-6);
  });
});

describe('interpolation helpers', () => {
  it('lerp interpolates endpoints', () => {
    expect(lerp(0, 10, 0)).toBe(0);
    expect(lerp(0, 10, 1)).toBe(10);
    expect(lerp(0, 10, 0.5)).toBe(5);
  });

  it('smoothAlpha stays within [0,1] and is 0 at dt=0', () => {
    expect(smoothAlpha(0.2, 0)).toBe(0);
    const a = smoothAlpha(0.2, 1 / 60);
    expect(a).toBeGreaterThan(0);
    expect(a).toBeLessThanOrEqual(1);
  });
});
