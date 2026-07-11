import { describe, it, expect } from 'vitest';
import { computeMood, moodExpression } from './mascot-mood';

describe('computeMood', () => {
  it('returns relaxed when nothing is going on', () => {
    expect(computeMood({ total: 3, active: 0, error: 0, inbox: 0 })).toBe('relaxed');
  });

  it('returns focused when staff are active and inbox is empty', () => {
    expect(computeMood({ total: 3, active: 2, error: 0, inbox: 0 })).toBe('focused');
  });

  it('returns poke when the inbox is non-empty', () => {
    expect(computeMood({ total: 3, active: 0, error: 0, inbox: 1 })).toBe('poke');
  });

  it('returns alert when there is an error, regardless of other signals', () => {
    expect(computeMood({ total: 3, active: 0, error: 1, inbox: 0 })).toBe('alert');
  });

  it('precedence: error beats poke', () => {
    expect(computeMood({ total: 3, active: 1, error: 1, inbox: 5 })).toBe('alert');
  });

  it('precedence: poke beats focused', () => {
    expect(computeMood({ total: 3, active: 2, error: 0, inbox: 1 })).toBe('poke');
  });
});

describe('moodExpression', () => {
  it('maps every mood to a DuDu face and label id', () => {
    const faces: Record<string, string> = {
      relaxed: 'idle',
      focused: 'writing',
      alert: 'concerned',
      poke: 'curious',
    };
    (['relaxed', 'focused', 'alert', 'poke'] as const).forEach((mood) => {
      const expr = moodExpression(mood);
      expect(expr.face).toBe(faces[mood]);
      expect(expr.labelId).toBe(`mascot.mood.${mood}`);
    });
  });
});
