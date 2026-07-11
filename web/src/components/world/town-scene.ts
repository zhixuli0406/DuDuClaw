/**
 * The "town" scene — a small isometric city (the world-stage centrepiece).
 *
 * A deterministic grid of streets (horizontal + vertical road bands) over a
 * grass base, with pavements auto-derived where grass meets road. Each block
 * between the streets carries extruded buildings (≥6 roof colours, varied
 * storeys), trees and street lamps; the central block is a plaza with a water
 * pool and planters. Cars are *not* baked here — they are ambient sprites the
 * renderer drives along the horizontal lanes (see `traffic.ts`).
 *
 * The layout is computed **once** at module load (fixed seed) so the town is
 * stable across mounts and the agent-placement helpers can index a constant set
 * of building doors. `buildTown()` ignores `agentCount` — the town is fixed; only
 * how many agents walk its streets varies.
 */
import {
  type FurniturePiece,
  type PlacementInput,
  type RoomDefinition,
  type ScenePlacement,
  type TileCoord,
  TileCode,
} from './types';

const SIZE = 20;
/** Road band coordinates (2-wide bands) splitting the map into a 3×3 of blocks. */
const ROAD_ROWS = new Set([4, 5, 13, 14]);
const ROAD_COLS = new Set([4, 5, 13, 14]);

/** Deterministic block interiors (grass spans between the road bands). */
const BANDS: ReadonlyArray<{ lo: number; hi: number }> = [
  { lo: 0, hi: 3 },
  { lo: 6, hi: 12 },
  { lo: 15, hi: 19 },
];

interface TownLayout {
  readonly matrix: TileCode[][];
  readonly furniture: FurniturePiece[];
  readonly doors: ReadonlyArray<TileCoord>;
  readonly plaza: TileCoord;
  readonly spawn: TileCoord;
}

/** A tiny seeded LCG so the town is identical every load. */
function makeRandom(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0x100000000;
  };
}

function isRoad(matrix: TileCode[][], x: number, y: number): boolean {
  return matrix[y]?.[x] === TileCode.Road;
}

function buildLayout(): TownLayout {
  const matrix: TileCode[][] = [];
  for (let y = 0; y < SIZE; y++) {
    const row: TileCode[] = [];
    for (let x = 0; x < SIZE; x++) {
      row.push(ROAD_ROWS.has(y) || ROAD_COLS.has(x) ? TileCode.Road : TileCode.Grass);
    }
    matrix.push(row);
  }
  // Grass touching a road becomes pavement (a walkable border).
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      if (matrix[y][x] !== TileCode.Grass) continue;
      if (isRoad(matrix, x - 1, y) || isRoad(matrix, x + 1, y) || isRoad(matrix, x, y - 1) || isRoad(matrix, x, y + 1)) {
        matrix[y][x] = TileCode.Pavement;
      }
    }
  }

  const rand = makeRandom(0x5eed1234);
  const furniture: FurniturePiece[] = [];
  const doors: TileCoord[] = [];
  const centre = BANDS[1]; // the middle band → plaza
  const plaza: TileCoord = {
    x: Math.round((centre.lo + centre.hi) / 2),
    y: Math.round((centre.lo + centre.hi) / 2),
  };

  for (let by = 0; by < BANDS.length; by++) {
    for (let bx = 0; bx < BANDS.length; bx++) {
      const bandX = BANDS[bx];
      const bandY = BANDS[by];
      const isCentre = bx === 1 && by === 1;

      if (isCentre) {
        // Central plaza: a water pool ringed by planters + a fountain.
        for (let y = plaza.y - 1; y <= plaza.y + 1; y++) {
          for (let x = plaza.x - 1; x <= plaza.x + 1; x++) {
            if (matrix[y]?.[x] === TileCode.Grass) matrix[y][x] = TileCode.Water;
          }
        }
        furniture.push({ kind: 'fountain', x: plaza.x, y: plaza.y });
        furniture.push({ kind: 'tree', x: bandX.lo, y: bandY.lo, variant: 0 });
        furniture.push({ kind: 'tree', x: bandX.hi, y: bandY.lo, variant: 1 });
        furniture.push({ kind: 'tree', x: bandX.lo, y: bandY.hi, variant: 2 });
        furniture.push({ kind: 'tree', x: bandX.hi, y: bandY.hi, variant: 1 });
        continue;
      }

      // A street lamp on the block's north-west pavement corner.
      furniture.push({ kind: 'streetlamp', x: bandX.lo, y: bandY.lo });

      // Two–three buildings tucked along the block's interior, on grass tiles.
      const slots: TileCoord[] = [
        { x: bandX.lo + 1, y: bandY.lo + 1 },
        { x: bandX.hi - 1, y: bandY.lo + 1 },
        { x: bandX.lo + 1, y: bandY.hi - 1 },
      ];
      const count = 2 + Math.floor(rand() * 2); // 2 or 3
      for (let i = 0; i < count && i < slots.length; i++) {
        const slot = slots[i];
        if (matrix[slot.y]?.[slot.x] !== TileCode.Grass) continue;
        furniture.push({
          kind: 'building',
          x: slot.x,
          y: slot.y,
          variant: Math.floor(rand() * 8),
          level: 1 + Math.floor(rand() * 3), // 1..3 storeys
        });
        // The door tile is the pavement/grass just south of the building.
        doors.push({ x: slot.x, y: slot.y + 1 });
      }
      // One tree per block for greenery.
      furniture.push({ kind: 'tree', x: bandX.hi - 1, y: bandY.hi - 1, variant: Math.floor(rand() * 3) });
    }
  }

  return { matrix, furniture, doors, plaza, spawn: { x: plaza.x, y: centre.hi } };
}

/** Computed once — the town is deterministic and fixed. */
const LAYOUT: TownLayout = buildLayout();

/** Build the town room (agent count does not change the fixed cityscape). */
export function buildTown(_agentCount: number): RoomDefinition {
  return { key: 'town', matrix: LAYOUT.matrix, furniture: LAYOUT.furniture, spawn: LAYOUT.spawn };
}

/** Number of building doors agents can be routed to (≥1; falls back to plaza). */
export function townDoorCount(): number {
  return LAYOUT.doors.length;
}

/**
 * Place one agent in the town:
 *  - active & busy → walking to their own building's door
 *  - active & idle / paused → strolling the central plaza
 *  - terminated → dimmed near the plaza (empty read)
 */
export function placeTown(input: PlacementInput): ScenePlacement {
  const { index, status, busy } = input;
  const plaza = LAYOUT.plaza;

  if (status === 'active' && busy && LAYOUT.doors.length > 0) {
    const door = LAYOUT.doors[index % LAYOUT.doors.length];
    return { x: door.x, y: door.y, action: 'walking', facing: index % 2 === 0 ? 'right' : 'left', dimmed: false };
  }

  // Idle / paused / terminated: spread around the plaza deterministically.
  const ring = 2 + (index % 2);
  const angle = (index * 2.399963) % (Math.PI * 2); // golden-angle spread
  const x = plaza.x + Math.cos(angle) * ring;
  const y = plaza.y + Math.sin(angle) * ring * 0.6;
  return {
    x,
    y,
    action: 'idle',
    facing: Math.cos(angle) >= 0 ? 'right' : 'left',
    dimmed: status === 'terminated',
  };
}
