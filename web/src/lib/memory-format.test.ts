import { describe, it, expect } from 'vitest';
import {
  parsePredictionMemory,
  toPercent,
  summarizeMemory,
  memoryLayerMessageId,
  memorySourceMessageId,
} from './memory-format';

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

describe('summarizeMemory', () => {
  it('returns short single-line content untouched', () => {
    expect(summarizeMemory('客戶的合約報價是三萬元')).toBe('客戶的合約報價是三萬元');
  });

  // WP15 — a memory written as "heading / detail / detail" must not show up on
  // the row as just its heading.
  it('joins every non-empty line so the detail survives onto the row', () => {
    expect(summarizeMemory('\n\n客戶資料更新\n公司: 嘟嘟數位\n電話: 0912-345-678')).toBe(
      '客戶資料更新 · 公司: 嘟嘟數位 · 電話: 0912-345-678',
    );
  });

  it('collapses runs of whitespace', () => {
    expect(summarizeMemory('報價   是    三萬元')).toBe('報價 是 三萬元');
  });

  it('cuts a long memory at a sentence boundary when one fits', () => {
    const long = `${'一'.repeat(30)}。${'二'.repeat(80)}`;
    const out = summarizeMemory(long, 60);
    expect(out).toBe(`${'一'.repeat(30)}。`);
    expect(out.length).toBeLessThan(long.length);
  });

  it('hard-cuts with an ellipsis when no usable sentence boundary exists', () => {
    const long = '合'.repeat(200);
    const out = summarizeMemory(long, 40);
    expect(out.endsWith('…')).toBe(true);
    expect(Array.from(out).length).toBe(41);
  });

  it('never splits a surrogate pair (emoji stay whole)', () => {
    const out = summarizeMemory('🐾'.repeat(100), 10);
    // 10 emoji + the ellipsis, and every emoji is still a complete codepoint.
    expect(Array.from(out)).toHaveLength(11);
    expect(out.includes('�')).toBe(false);
    expect(out.startsWith('🐾🐾')).toBe(true);
  });

  it('keeps a regional-indicator flag whole at the budget boundary', () => {
    // 🇹🇼 is two codepoints; a codepoint budget of 90 would cut between them
    // and leave a bare 🇹 behind. As one grapheme the line fits exactly.
    const line = `${'台'.repeat(89)}🇹🇼`;
    const out = summarizeMemory(line, 90);
    expect(out).toBe(line);
    expect(out).toContain('🇹🇼');
    expect(out.endsWith('…')).toBe(false);
  });

  it('keeps a ZWJ family sequence whole at the budget boundary', () => {
    // 👨‍👩‍👧‍👦 is 7 codepoints (4 emoji + 3 ZWJ) but one grapheme, so 86 + 1
    // fits where a codepoint budget of 90 would have shredded it into pieces.
    const line = `${'家'.repeat(86)}👨‍👩‍👧‍👦`;
    const out = summarizeMemory(line, 90);
    expect(out).toBe(line);
    // The sequence survives as a unit at the very end, not as a severed prefix.
    expect(out.endsWith('👨‍👩‍👧‍👦')).toBe(true);
    expect(out.endsWith('…')).toBe(false);
  });

  it('cuts between graphemes, never inside one, when truncation does happen', () => {
    const line = '🇹🇼'.repeat(20);
    const out = summarizeMemory(line, 5);
    expect(out).toBe(`${'🇹🇼'.repeat(5)}…`);
  });
});

describe('memoryLayerMessageId', () => {
  it('maps the cognitive layers to end-user wording keys', () => {
    expect(memoryLayerMessageId('episodic')).toBe('memory.layer.episodic');
    expect(memoryLayerMessageId('SEMANTIC')).toBe('memory.layer.semantic');
    expect(memoryLayerMessageId('procedural')).toBe('memory.layer.procedural');
  });

  it('returns null for missing or unknown layers so no chip renders', () => {
    expect(memoryLayerMessageId(undefined)).toBeNull();
    expect(memoryLayerMessageId('')).toBeNull();
    expect(memoryLayerMessageId('archival')).toBeNull();
  });
});

describe('memorySourceMessageId', () => {
  it('prefers the source_event mapping', () => {
    expect(memorySourceMessageId('footprint_distill')).toBe('memory.source.footprint');
    expect(memorySourceMessageId('user_profile_consolidation')).toBe('memory.source.profile');
    expect(memorySourceMessageId('decision_resolve')).toBe('memory.source.decision');
  });

  it('falls back to tags when the origin lives there instead', () => {
    expect(memorySourceMessageId(undefined, ['reflexion'])).toBe('memory.source.reflexion');
    expect(memorySourceMessageId('unmapped_pipeline', ['footprint-distill'])).toBe(
      'memory.source.footprint',
    );
  });

  it('falls back to the conversation wording when nothing is stamped', () => {
    expect(memorySourceMessageId()).toBe('memory.source.conversation');
    expect(memorySourceMessageId('brand_new_pipeline', ['misc'])).toBe('memory.source.conversation');
  });
});
