import { describe, it, expect } from 'vitest';
import { VISEMES } from '@/components/mascot';
import { REST_VISEME, sampleViseme } from './viseme-sampler';

describe('sampleViseme', () => {
  it('re-exports REST as the seed shape', () => {
    expect(REST_VISEME).toEqual(VISEMES.REST);
  });

  it('opens the mouth for a vowel-ending chunk', () => {
    const out = sampleViseme(VISEMES.REST, 'aaaa');
    expect(out.openness).toBeGreaterThan(VISEMES.REST.openness);
  });

  it('collapses toward REST on a punctuation-only tail', () => {
    const wide = sampleViseme(VISEMES.REST, 'yeeee'); // open first
    const closed = sampleViseme(wide, '...');
    expect(closed.openness).toBeLessThan(wide.openness);
  });

  it('animates CJK deterministically (moves the mouth, same input → same shape)', () => {
    const a = sampleViseme(VISEMES.REST, '你好嗎');
    const b = sampleViseme(VISEMES.REST, '你好嗎');
    expect(a).toEqual(b);
    expect(a.openness).toBeGreaterThan(0);
  });

  it('keeps openness/width within the clamped [0,1] range', () => {
    let v = VISEMES.REST;
    for (const c of 'The quick brown fox 你好 123!') {
      v = sampleViseme(v, c);
      expect(v.openness).toBeGreaterThanOrEqual(0);
      expect(v.openness).toBeLessThanOrEqual(1);
      expect(v.width).toBeGreaterThanOrEqual(0);
      expect(v.width).toBeLessThanOrEqual(1);
    }
  });
});
