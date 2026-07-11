/**
 * Ambient traffic model for the town scene (pure, unit-tested).
 *
 * Cars cruise along the town's *horizontal* road rows (they travel east/west
 * along a fixed matrix row). The model is deliberately renderer-free:
 * {@link advanceCar} moves a car one frame and wraps it around the
 * street when it drives off an edge, so the streets stay populated with zero
 * per-frame allocation and no spawn/retire churn (a wrap is cheaper than a
 * destroy+respawn and reads identically at ambient scale).
 *
 * The renderer owns the sprites; this module owns only the numbers.
 */
import type { TileCode } from './types';
import { TileCode as Tile } from './types';

/** Max cars on screen at once (perf ceiling — task cap is 12). */
export const MAX_CARS = 12;

/** How many cars ride each horizontal lane. */
export const CARS_PER_LANE = 3;

/** Warm car tints (0xRRGGBB). Assigned round-robin so a lane isn't monochrome. */
export const CAR_TINTS: ReadonlyArray<number> = [
  0xe86a5a, 0xf2a154, 0x4a90d9, 0x6aa86a, 0xb07ad0, 0xf2c14e, 0x5b616b, 0xef7fa2,
];

/** A single lane: a road row and the direction cars travel along it (+1 / -1). */
export interface Lane {
  readonly row: number;
  readonly direction: 1 | -1;
}

/** One live car's mutable state. Position `x` is a fractional tile column. */
export interface CarState {
  x: number;
  readonly row: number;
  readonly direction: 1 | -1;
  /** Tiles per second. */
  readonly speed: number;
  readonly tint: number;
}

/**
 * Find the horizontal lanes in a matrix: rows that are *fully* road across their
 * width (through-streets), not the short cross-street stubs. Alternating rows
 * travel opposite directions so traffic reads as oncoming lanes.
 */
export function findHorizontalLanes(matrix: ReadonlyArray<ReadonlyArray<TileCode>>): Lane[] {
  const lanes: Lane[] = [];
  const width = matrix[0]?.length ?? 0;
  if (width === 0) return lanes;
  let flip = 0;
  for (let row = 0; row < matrix.length; row++) {
    const line = matrix[row];
    let roadCount = 0;
    for (let x = 0; x < width; x++) if (line[x] === Tile.Road) roadCount++;
    // A through-street: at least ~85% of the row is road.
    if (roadCount >= width * 0.85) {
      lanes.push({ row, direction: flip % 2 === 0 ? 1 : -1 });
      flip++;
    }
  }
  return lanes;
}

/**
 * Advance a car's fractional column one frame and wrap it around the street.
 * Pure — returns the next `x` (does not mutate). `min`/`max` bound the drivable
 * span (a little past each edge so the wrap is off-screen). Deterministic given
 * its inputs, so it is unit-tested directly.
 */
export function advanceCar(
  x: number,
  direction: 1 | -1,
  speed: number,
  dtSeconds: number,
  min: number,
  max: number,
): number {
  const span = max - min;
  if (span <= 0) return x;
  let next = x + direction * speed * dtSeconds;
  if (next > max) next = min + ((next - min) % span);
  else if (next < min) next = max - ((min - next) % span);
  return next;
}

/**
 * Build the initial car set for a set of lanes, spaced evenly with a stable
 * (seeded) jitter so the streets don't look like a metronome. Capped at
 * {@link MAX_CARS}. Deterministic for a given seed → testable, and the town
 * looks the same each mount.
 */
export function seedCars(lanes: ReadonlyArray<Lane>, width: number, seed = 0x1357acef): CarState[] {
  const cars: CarState[] = [];
  let s = seed >>> 0;
  const rand = (): number => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0x100000000;
  };
  const spacing = CARS_PER_LANE > 0 ? width / CARS_PER_LANE : width;
  let tintCursor = 0;
  for (const lane of lanes) {
    for (let i = 0; i < CARS_PER_LANE; i++) {
      if (cars.length >= MAX_CARS) return cars;
      cars.push({
        x: i * spacing + rand() * spacing,
        row: lane.row,
        direction: lane.direction,
        speed: 1.3 + rand() * 1.2,
        tint: CAR_TINTS[tintCursor++ % CAR_TINTS.length],
      });
    }
  }
  return cars;
}
