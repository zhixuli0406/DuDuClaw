/**
 * Declarative office room (V8-T8.2).
 *
 * A single open-plan office: a tile matrix ringed by walls, a rug, one desk per
 * employee (capped at {@link DESK_CAP}), and the fixed interactive props —
 * bulletin board, whiteboard, coffee machine, front door, and the vault. The
 * layout is *declared*, not drawn imperatively: `buildOffice(count)` returns a
 * `RoomDefinition` the renderer walks.
 *
 * The five-colour palette is read from CSS tokens at build time
 * ({@link resolveRoomPalette}) so it re-themes with light/dark; a hard-coded
 * fallback keeps jsdom / SSR (where `getComputedStyle` yields '') from crashing.
 */
import {
  type FurniturePiece,
  type PlacementInput,
  type RoomDefinition,
  type RoomPalette,
  type SceneDefinition,
  type SceneKey,
  type ScenePlacement,
  TileCode,
} from './types';
import { buildTown, placeTown } from './town-scene';
import { buildLounge, placeLounge } from './lounge-scene';

/** Max desks shown before overflow agents share the floor near the coffee machine. */
export const DESK_CAP = 12;

/** Interior desk grid: 4 columns × 3 rows of desks (= 12), agents sit south of each. */
const DESK_COLS = [2, 4, 6, 8];
const DESK_ROWS = [2, 4, 6];

const GRID_W = 11;
const GRID_H = 10;

/** Fixed props anchored on the perimeter (independent of employee count). */
const FIXED_FURNITURE: ReadonlyArray<FurniturePiece> = [
  { kind: 'bulletin', x: 1, y: 0, object: 'bulletin' },
  { kind: 'window', x: 5, y: 0 },
  { kind: 'whiteboard', x: 9, y: 0, object: 'whiteboard' },
  { kind: 'door', x: 5, y: 9, object: 'door' },
  { kind: 'vault', x: 9, y: 8, object: 'vault' },
  { kind: 'coffee', x: 1, y: 8, object: 'coffee' },
  { kind: 'cabinet', x: 9, y: 5 },
  { kind: 'plant', x: 1, y: 1 },
  { kind: 'plant', x: 9, y: 1 },
  { kind: 'plant', x: 3, y: 8 },
  { kind: 'plant', x: 7, y: 8 },
];

/** Build the base tile matrix: walls on the top/left edges, floor + a centre rug. */
function baseMatrix(): TileCode[][] {
  const m: TileCode[][] = [];
  for (let y = 0; y < GRID_H; y++) {
    const row: TileCode[] = [];
    for (let x = 0; x < GRID_W; x++) {
      // Back walls along the top row and left column (the visible iso walls).
      if (y === 0 || x === 0) row.push(TileCode.Wall);
      else row.push(TileCode.Floor);
    }
    m.push(row);
  }
  // A rug under the central desks for warmth.
  for (let y = 2; y <= 6; y++) {
    for (let x = 2; x <= 8; x++) {
      if (m[y]?.[x] === TileCode.Floor && (x + y) % 2 === 0) m[y][x] = TileCode.Rug;
    }
  }
  return m;
}

/**
 * Compute each employee's desk placement. Desks fill the 4×3 grid in reading
 * order; overflow employees (beyond {@link DESK_CAP}) don't get a desk and are
 * placed near the coffee machine by the controller.
 */
export function deskTiles(): ReadonlyArray<{ x: number; y: number }> {
  const tiles: Array<{ x: number; y: number }> = [];
  for (const y of DESK_ROWS) {
    for (const x of DESK_COLS) tiles.push({ x, y });
  }
  return tiles;
}

/** The tile an agent stands on when seated at desk `index` (one south of the desk). */
export function seatTileFor(index: number): { x: number; y: number } {
  const desks = deskTiles();
  const d = desks[index % desks.length];
  return { x: d.x, y: d.y + 1 };
}

/** The coffee-machine loiter tile (idle / overflow agents gather here). */
export function coffeeTile(): { x: number; y: number } {
  return { x: 2, y: 8 };
}

/**
 * Build the office for `agentCount` employees. Desks are capped; the matrix and
 * fixed props are constant. `agentCount` only bounds how many desks are emitted
 * as furniture so an empty office isn't cluttered with unused desks.
 */
export function buildOffice(agentCount: number): RoomDefinition {
  const desks = deskTiles();
  const shown = Math.min(Math.max(0, agentCount), DESK_CAP);
  const deskFurniture: FurniturePiece[] = desks.slice(0, shown).map((d, i) => ({
    kind: 'desk',
    x: d.x,
    y: d.y,
    deskIndex: i,
    object: 'agent',
  }));

  return {
    key: 'office',
    matrix: baseMatrix(),
    furniture: [...deskFurniture, ...FIXED_FURNITURE],
    spawn: coffeeTile(),
  };
}

/** Hard-coded fallback palette (warm office) for jsdom / token-less contexts. */
const FALLBACK_PALETTE: RoomPalette = {
  floor: 0xf2e6d0,
  floorAlt: 0xe8d7ba,
  wall: 0xd9c6a8,
  wallSide: 0xc4ad8a,
  rug: 0xd8c8a0,
};

/** Map a CSS custom-property name to a resolved 0xRRGGBB via the browser. */
function readCssColor(varName: string, probe: HTMLElement): number | null {
  const raw = getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
  if (!raw) return null;
  // Let the browser resolve oklch()/hsl()/etc. to concrete rgb by round-tripping
  // through a computed `color`. jsdom returns the input unchanged → we fall back.
  probe.style.color = '';
  probe.style.color = raw;
  const resolved = getComputedStyle(probe).color; // "rgb(r, g, b)" in real browsers
  const m = resolved.match(/rgba?\(([^)]+)\)/);
  if (!m) return null;
  const parts = m[1].split(',').map((s) => parseFloat(s.trim()));
  if (parts.length < 3 || parts.some((n) => Number.isNaN(n))) return null;
  const [r, g, b] = parts;
  return ((r & 0xff) << 16) | ((g & 0xff) << 8) | (b & 0xff);
}

/**
 * Resolve the room palette from CSS tokens (re-read on theme change). Any token
 * that fails to resolve (jsdom, missing var) falls back to {@link FALLBACK_PALETTE}
 * so the renderer always gets five valid colours.
 */
export function resolveRoomPalette(): RoomPalette {
  if (typeof document === 'undefined') return FALLBACK_PALETTE;
  const probe = document.createElement('span');
  probe.style.display = 'none';
  document.body.appendChild(probe);
  try {
    const pick = (varName: string, fallback: number): number =>
      readCssColor(varName, probe) ?? fallback;
    return {
      floor: pick('--agent-9a', FALLBACK_PALETTE.floor),
      floorAlt: pick('--agent-9b', FALLBACK_PALETTE.floorAlt),
      wall: pick('--agent-1a', FALLBACK_PALETTE.wall),
      wallSide: pick('--agent-1b', FALLBACK_PALETTE.wallSide),
      rug: pick('--agent-3a', FALLBACK_PALETTE.rug),
      ...SCENE_TILE_COLORS,
    };
  } finally {
    probe.remove();
  }
}

/**
 * Scene-tile colours (town / lounge). These are authored constants rather than
 * theme tokens: asphalt, grass and water must stay legible and recognisable in
 * both light and dark, so they don't re-tint with the theme.
 */
const SCENE_TILE_COLORS = {
  road: 0x565b66,
  pavement: 0xc2c7cf,
  grass: 0x7cb473,
  water: 0x5aa9d6,
  carpet: 0xcf9f6f,
} as const;

/** Agent gradient stop `--agent-{n}a` as 0xRRGGBB (for sprite body tint). */
export function resolveAgentTint(tintIndex: number): number {
  if (typeof document === 'undefined') return 0xf2a154;
  const probe = document.createElement('span');
  probe.style.display = 'none';
  document.body.appendChild(probe);
  try {
    const n = ((tintIndex - 1) % 10) + 1;
    return readCssColor(`--agent-${n}a`, probe) ?? 0xf2a154;
  } finally {
    probe.remove();
  }
}

// ---- Office placement ------------------------------------------------------

/**
 * Place one agent in the office (the original behaviour §8.2, now expressed as a
 * scene placer): terminated → dimmed at its desk; paused / overflow → loitering
 * by the coffee machine; active → seated at its desk.
 */
export function placeOffice(input: PlacementInput): ScenePlacement {
  const { index, status } = input;
  const overflow = index >= DESK_CAP;
  const seat = seatTileFor(index);
  const coffee = coffeeTile();
  if (status === 'terminated') {
    return { x: seat.x, y: seat.y, action: 'sitting', facing: 'right', dimmed: true };
  }
  if (status === 'paused' || overflow) {
    return {
      x: coffee.x + (index % 3) * 0.6,
      y: coffee.y - (index % 2) * 0.5,
      action: 'idle',
      facing: 'right',
      dimmed: false,
    };
  }
  return { x: seat.x, y: seat.y, action: 'sitting', facing: 'left', dimmed: false };
}

// ---- Scene registry --------------------------------------------------------

/** All built-in scenes, in menu order. Index 0 is the default. */
export const SCENES: ReadonlyArray<SceneDefinition> = [
  {
    key: 'office',
    nameId: 'world.scene.office.name',
    descId: 'world.scene.office.desc',
    ambient: 'none',
    cacheGround: false,
    build: buildOffice,
    place: placeOffice,
  },
  {
    key: 'town',
    nameId: 'world.scene.town.name',
    descId: 'world.scene.town.desc',
    ambient: 'cars',
    cacheGround: true,
    build: buildTown,
    place: placeTown,
  },
  {
    key: 'lounge',
    nameId: 'world.scene.lounge.name',
    descId: 'world.scene.lounge.desc',
    ambient: 'none',
    cacheGround: false,
    build: buildLounge,
    place: placeLounge,
  },
];

export const DEFAULT_SCENE_KEY: SceneKey = 'office';

/** Resolve a scene by key, falling back to the default for unknown keys. */
export function getScene(key: string | null | undefined): SceneDefinition {
  return SCENES.find((s) => s.key === key) ?? SCENES[0];
}

/** localStorage key remembering the chosen scene (shared by stage + static). */
export const SCENE_STORAGE_KEY = 'duduclaw:world:scene';

/** Read the persisted scene key (default when unset / unavailable). */
export function readSceneKey(): SceneKey {
  if (typeof localStorage === 'undefined') return DEFAULT_SCENE_KEY;
  try {
    const v = localStorage.getItem(SCENE_STORAGE_KEY);
    return SCENES.some((s) => s.key === v) ? (v as SceneKey) : DEFAULT_SCENE_KEY;
  } catch {
    return DEFAULT_SCENE_KEY;
  }
}

/** Persist the chosen scene key (best-effort; private mode / quota tolerated). */
export function writeSceneKey(key: SceneKey): void {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(SCENE_STORAGE_KEY, key);
  } catch {
    /* non-fatal — the choice just won't persist */
  }
}
