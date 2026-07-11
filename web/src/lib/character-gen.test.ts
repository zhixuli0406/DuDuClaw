import { describe, it, expect } from 'vitest';
import {
  characterFor,
  type CharacterAccessory,
} from './character-gen';

const ACCESSORIES: CharacterAccessory[] = [
  'antenna',
  'bow',
  'glasses',
  'cap',
  'scarf',
  'flower',
];

describe('characterFor', () => {
  it('is deterministic — same id always yields the same traits', () => {
    const a = characterFor('marketing-bot');
    const b = characterFor('marketing-bot');
    expect(a).toEqual(b);
  });

  it('produces different traits for different ids (at least tint or accessory)', () => {
    const a = characterFor('alpha');
    const b = characterFor('bravo');
    // Not guaranteed to differ on every field, but the pair should not be identical.
    expect(a).not.toEqual(b);
  });

  it('keeps tintIndex within 1..10', () => {
    for (let i = 0; i < 500; i++) {
      const { tintIndex } = characterFor(`agent-${i}`);
      expect(tintIndex).toBeGreaterThanOrEqual(1);
      expect(tintIndex).toBeLessThanOrEqual(10);
      expect(Number.isInteger(tintIndex)).toBe(true);
    }
  });

  it('always picks a known accessory', () => {
    for (let i = 0; i < 500; i++) {
      const { accessory } = characterFor(`worker-${i}`);
      expect(ACCESSORIES).toContain(accessory);
    }
  });

  it('keeps blinkSeedMs within [0, 3600)', () => {
    for (let i = 0; i < 500; i++) {
      const { blinkSeedMs } = characterFor(`node-${i}`);
      expect(blinkSeedMs).toBeGreaterThanOrEqual(0);
      expect(blinkSeedMs).toBeLessThan(3600);
    }
  });

  it('handles empty / nullish ids without throwing', () => {
    expect(() => characterFor('')).not.toThrow();
    expect(() => characterFor(null)).not.toThrow();
    expect(() => characterFor(undefined)).not.toThrow();
    // Empty and nullish share the same fallback seed.
    expect(characterFor('')).toEqual(characterFor(null));
  });

  it('spreads tints roughly uniformly across the 10 buckets', () => {
    const N = 2000;
    const counts = new Array(11).fill(0);
    for (let i = 0; i < N; i++) counts[characterFor(`spread-${i}`).tintIndex]++;
    for (let n = 1; n <= 10; n++) {
      // Expected N/10 = 200 each; allow a generous band (no bucket empty, none dominates).
      expect(counts[n]).toBeGreaterThan(N / 10 / 3);
      expect(counts[n]).toBeLessThan((N / 10) * 3);
    }
  });

  it('makes antenna the most common accessory (weighted)', () => {
    const N = 3000;
    const tally: Record<string, number> = {};
    for (let i = 0; i < N; i++) {
      const a = characterFor(`acc-${i}`).accessory;
      tally[a] = (tally[a] ?? 0) + 1;
    }
    const antenna = tally.antenna ?? 0;
    for (const acc of ACCESSORIES) {
      if (acc === 'antenna') continue;
      expect(antenna).toBeGreaterThan(tally[acc] ?? 0);
    }
  });
});
