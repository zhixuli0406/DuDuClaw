/**
 * The "lounge" scene — a small break room where off-duty agents gather.
 *
 * Intentionally intimate: a warm carpet zone ringed by walls, a coffee machine,
 * two sofas and a board-game table. Only *resting* agents appear here — an agent
 * that is actively working (a live non-idle signal) is "in the office" and is
 * hidden from the lounge, so `placeLounge` returns `null` for it.
 */
import {
  type FurniturePiece,
  type PlacementInput,
  type RoomDefinition,
  type ScenePlacement,
  type TileCoord,
  TileCode,
} from './types';

const GRID_W = 11;
const GRID_H = 9;

/** Gather spots agents loiter around, cycled by roster index. */
const GATHER_TILES: ReadonlyArray<TileCoord> = [
  { x: 3, y: 5 }, // by the left sofa
  { x: 4, y: 6 },
  { x: 7, y: 5 }, // board-game table
  { x: 8, y: 6 },
  { x: 2, y: 3 }, // by the coffee machine
  { x: 3, y: 3 },
];

const FURNITURE: ReadonlyArray<FurniturePiece> = [
  { kind: 'coffeeMachine', x: 1, y: 2, object: 'coffee' },
  { kind: 'sofa', x: 3, y: 4 },
  { kind: 'sofa', x: 3, y: 6 },
  { kind: 'boardgame', x: 7, y: 4 },
  { kind: 'plant', x: 9, y: 1 },
  { kind: 'plant', x: 1, y: 7 },
  { kind: 'window', x: 5, y: 0 },
  { kind: 'door', x: 9, y: 8, object: 'door' },
];

function buildMatrix(): TileCode[][] {
  const m: TileCode[][] = [];
  for (let y = 0; y < GRID_H; y++) {
    const row: TileCode[] = [];
    for (let x = 0; x < GRID_W; x++) {
      if (y === 0 || x === 0) row.push(TileCode.Wall);
      else row.push(TileCode.Floor);
    }
    m.push(row);
  }
  // A cosy central carpet.
  for (let y = 3; y <= 6; y++) {
    for (let x = 2; x <= 8; x++) {
      if (m[y]?.[x] === TileCode.Floor) m[y][x] = TileCode.Carpet;
    }
  }
  return m;
}

const MATRIX = buildMatrix();

/** Build the lounge room (fixed layout; agent count is irrelevant). */
export function buildLounge(_agentCount: number): RoomDefinition {
  return { key: 'lounge', matrix: MATRIX, furniture: FURNITURE.slice(), spawn: { x: 3, y: 3 } };
}

/**
 * Place one agent in the lounge. Busy (actively-working) agents are elsewhere →
 * `null`. Resting agents gather around the sofas / coffee machine; a terminated
 * agent sits dimmed on a sofa.
 */
export function placeLounge(input: PlacementInput): ScenePlacement | null {
  const { index, status, busy } = input;
  if (status === 'active' && busy) return null; // working → not in the lounge
  const spot = GATHER_TILES[index % GATHER_TILES.length];
  return {
    x: spot.x,
    y: spot.y,
    action: 'idle',
    facing: index % 2 === 0 ? 'right' : 'left',
    dimmed: status === 'terminated',
  };
}
