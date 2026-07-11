import { describe, it, expect, beforeEach } from 'vitest';
import {
  SCENES,
  getScene,
  readSceneKey,
  writeSceneKey,
  DEFAULT_SCENE_KEY,
  SCENE_STORAGE_KEY,
} from './rooms';
import { advanceCar, findHorizontalLanes, seedCars, MAX_CARS } from './traffic';
import { TileCode, type SceneDefinition, type PlacementInput } from './types';

function inBounds(matrixW: number, matrixH: number, x: number, y: number): boolean {
  return x >= 0 && x <= matrixW - 1 && y >= 0 && y <= matrixH - 1;
}

describe('scene registry shape', () => {
  it('exposes exactly office / town / lounge in menu order', () => {
    expect(SCENES.map((s) => s.key)).toEqual(['office', 'town', 'lounge']);
    expect(SCENES[0].key).toBe(DEFAULT_SCENE_KEY);
  });

  it('every scene builds a rectangular matrix with an in-bounds spawn + furniture', () => {
    for (const scene of SCENES) {
      const room = scene.build(6);
      const h = room.matrix.length;
      const w = room.matrix[0]?.length ?? 0;
      expect(h).toBeGreaterThan(0);
      expect(w).toBeGreaterThan(0);
      // Rectangular: every row is the same width.
      for (const row of room.matrix) expect(row.length).toBe(w);
      // Spawn is inside the grid.
      expect(inBounds(w, h, room.spawn.x, room.spawn.y)).toBe(true);
      // Every furniture anchor is inside the grid.
      for (const f of room.furniture) {
        expect(inBounds(w, h, f.x, f.y)).toBe(true);
      }
    }
  });

  it('carries valid i18n ids + ambient/cache flags per scene', () => {
    const town = getScene('town');
    expect(town.ambient).toBe('cars');
    expect(town.cacheGround).toBe(true);
    expect(getScene('office').ambient).toBe('none');
    expect(getScene('office').cacheGround).toBe(false);
    for (const s of SCENES) {
      expect(s.nameId).toMatch(/^world\.scene\./);
      expect(s.descId).toMatch(/^world\.scene\./);
    }
  });

  it('getScene falls back to the default for unknown keys', () => {
    expect(getScene('does-not-exist').key).toBe(DEFAULT_SCENE_KEY);
    expect(getScene(null).key).toBe(DEFAULT_SCENE_KEY);
  });
});

describe('scene placement (reconcile is total + in-bounds)', () => {
  const statuses: PlacementInput['status'][] = ['active', 'paused', 'terminated'];

  it('placing a full roster in any scene never throws and stays in bounds', () => {
    for (const scene of SCENES) {
      const room = scene.build(20);
      const w = room.matrix[0].length;
      const h = room.matrix.length;
      for (let index = 0; index < 20; index++) {
        for (const status of statuses) {
          for (const busy of [true, false]) {
            const p = scene.place({ index, status, busy });
            if (p === null) continue; // scene hides this agent — legal
            expect(inBounds(w, h, p.x, p.y)).toBe(true);
            expect(['idle', 'walking', 'sitting']).toContain(p.action);
          }
        }
      }
    }
  });

  it('office seats active agents and loiters paused ones (unchanged §8.2)', () => {
    const office = getScene('office');
    const active = office.place({ index: 0, status: 'active', busy: true })!;
    expect(active.action).toBe('sitting');
    const paused = office.place({ index: 0, status: 'paused', busy: false })!;
    expect(paused.action).toBe('idle');
  });

  it('town walks busy workers to a door, idles the rest at the plaza', () => {
    const town = getScene('town');
    const busy = town.place({ index: 0, status: 'active', busy: true })!;
    expect(busy.action).toBe('walking');
    const resting = town.place({ index: 0, status: 'active', busy: false })!;
    expect(resting.action).toBe('idle');
  });

  it('lounge hides busy workers but shows resting / paused agents', () => {
    const lounge = getScene('lounge');
    expect(lounge.place({ index: 0, status: 'active', busy: true })).toBeNull();
    expect(lounge.place({ index: 0, status: 'active', busy: false })).not.toBeNull();
    expect(lounge.place({ index: 3, status: 'paused', busy: false })).not.toBeNull();
    const dead = lounge.place({ index: 1, status: 'terminated', busy: false })!;
    expect(dead.dimmed).toBe(true);
  });

  it('switching scenes reuses the same agent set without error (stable ids)', () => {
    // Reconcile the identical roster across every scene → the app only swaps the
    // placement fn, so this must be pure + total for the renderer's diff to work.
    const roster = Array.from({ length: 8 }, (_v, index) => index);
    const seen: SceneDefinition[] = [...SCENES];
    for (const scene of seen) {
      const placed = roster.map((index) => scene.place({ index, status: 'active', busy: index % 2 === 0 }));
      // At least one agent is shown in each scene for a mixed-busy roster.
      expect(placed.some((p) => p !== null)).toBe(true);
    }
  });
});

describe('ambient traffic model (town)', () => {
  it('finds the town horizontal lanes and seeds a capped car set', () => {
    const room = getScene('town').build(0);
    const lanes = findHorizontalLanes(room.matrix);
    expect(lanes.length).toBeGreaterThan(0);
    // Opposing directions alternate so lanes read as oncoming.
    if (lanes.length >= 2) expect(lanes[0].direction).not.toBe(lanes[1].direction);
    const cars = seedCars(lanes, room.matrix[0].length);
    expect(cars.length).toBeGreaterThan(0);
    expect(cars.length).toBeLessThanOrEqual(MAX_CARS);
    for (const car of cars) expect(car.speed).toBeGreaterThan(0);
  });

  it('advanceCar wraps around the street at either edge (deterministic)', () => {
    // Rightbound car past the max wraps back near the min.
    const wrappedR = advanceCar(9.5, 1, 10, 0.1, -2, 10);
    expect(wrappedR).toBeGreaterThanOrEqual(-2);
    expect(wrappedR).toBeLessThan(10);
    // Leftbound car past the min wraps up near the max.
    const wrappedL = advanceCar(-1.5, -1, 10, 0.1, -2, 10);
    expect(wrappedL).toBeLessThanOrEqual(10);
    expect(wrappedL).toBeGreaterThan(-2);
    // A normal step just moves by direction*speed*dt.
    expect(advanceCar(3, 1, 2, 0.5, -2, 10)).toBeCloseTo(4, 5);
  });

  it('office / lounge matrices carry no traffic lanes', () => {
    expect(findHorizontalLanes(getScene('office').build(0).matrix)).toHaveLength(0);
    expect(findHorizontalLanes(getScene('lounge').build(0).matrix)).toHaveLength(0);
  });
});

describe('scene persistence', () => {
  beforeEach(() => {
    localStorage.removeItem(SCENE_STORAGE_KEY);
  });

  it('defaults to the office when unset', () => {
    expect(readSceneKey()).toBe(DEFAULT_SCENE_KEY);
  });

  it('round-trips a chosen scene through localStorage', () => {
    writeSceneKey('town');
    expect(readSceneKey()).toBe('town');
    writeSceneKey('lounge');
    expect(readSceneKey()).toBe('lounge');
  });

  it('ignores a corrupt stored value', () => {
    localStorage.setItem(SCENE_STORAGE_KEY, 'garbage');
    expect(readSceneKey()).toBe(DEFAULT_SCENE_KEY);
  });
});

describe('town tiles include roads / grass / water (richness sanity)', () => {
  it('has all the expected town tile codes present', () => {
    const codes = new Set<TileCode>();
    for (const row of getScene('town').build(0).matrix) for (const c of row) codes.add(c);
    expect(codes.has(TileCode.Road)).toBe(true);
    expect(codes.has(TileCode.Pavement)).toBe(true);
    expect(codes.has(TileCode.Grass)).toBe(true);
    expect(codes.has(TileCode.Water)).toBe(true);
  });
});
