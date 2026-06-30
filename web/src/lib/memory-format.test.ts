import { describe, it, expect } from 'vitest';
import { parsePredictionMemory, toPercent } from './memory-format';

describe('parsePredictionMemory', () => {
  it('parses the prediction-deviation telemetry format', () => {
    const raw =
      'Prediction deviation: expected satisfaction 0.75, inferred 0.61 (delta 0.14). ' +
      'Topic surprise: 1.00. Corrections: yes. Follow-ups: no.';
    expect(parsePredictionMemory(raw)).toEqual({
      expected: 0.75,
      inferred: 0.61,
      delta: 0.14,
      surprise: 1.0,
      corrected: true,
      followUp: false,
    });
  });

  it('handles a negative delta and both flags no', () => {
    const raw =
      'Prediction deviation: expected satisfaction 0.30, inferred 0.47 (delta -0.17). ' +
      'Topic surprise: 0.00. Corrections: no. Follow-ups: no.';
    const p = parsePredictionMemory(raw);
    expect(p?.delta).toBe(-0.17);
    expect(p?.corrected).toBe(false);
    expect(p?.followUp).toBe(false);
  });

  it('tolerates surrounding whitespace and a missing trailing period', () => {
    const raw =
      '  Prediction deviation: expected satisfaction 0.5, inferred 0.5 (delta 0.0). ' +
      'Topic surprise: 0.2. Corrections: no. Follow-ups: yes  ';
    expect(parsePredictionMemory(raw)?.followUp).toBe(true);
  });

  it('returns null for ordinary memory content', () => {
    expect(parsePredictionMemory('使用者偏好用繁體中文回覆。')).toBeNull();
    expect(parsePredictionMemory('Prediction deviation: malformed')).toBeNull();
    expect(parsePredictionMemory('')).toBeNull();
  });
});

describe('toPercent', () => {
  it('rounds a ratio to a clamped whole percent', () => {
    expect(toPercent(0.61)).toBe(61);
    expect(toPercent(1)).toBe(100);
    expect(toPercent(0)).toBe(0);
    expect(toPercent(1.5)).toBe(100);
    expect(toPercent(-0.2)).toBe(0);
  });
});
