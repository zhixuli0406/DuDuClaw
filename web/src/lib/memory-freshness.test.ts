import { describe, it, expect } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import {
  DEFAULT_ARCHIVE_THRESHOLD,
  FRESHNESS_BANDS,
  FRESHNESS_IDS,
  FRESHNESS_STYLES,
  daysSinceRecall,
  daysToReach,
  freshnessBand,
  freshnessHintId,
  freshnessLabelId,
  retrievabilityAt,
} from './memory-freshness';

describe('freshnessBand', () => {
  it('classifies each state at and just below its boundary', () => {
    // The boundaries are the contract the gateway asserts too
    // (`MEMORY_FRESHNESS_BANDS` in handlers.rs); if these drift, the badge on a
    // row and the histogram bar it belongs to stop agreeing.
    expect(freshnessBand(1)).toBe('fresh');
    expect(freshnessBand(0.7)).toBe('fresh');
    expect(freshnessBand(0.699)).toBe('stable');
    expect(freshnessBand(0.4)).toBe('stable');
    expect(freshnessBand(0.399)).toBe('fading');
    expect(freshnessBand(0.15)).toBe('fading');
    expect(freshnessBand(0.149)).toBe('archiving');
    expect(freshnessBand(0)).toBe('archiving');
  });

  it('returns null rather than guessing when there is no figure', () => {
    // An older gateway sends no decay fields; the row must simply show no
    // badge instead of claiming a state it cannot know.
    expect(freshnessBand(undefined)).toBeNull();
    expect(freshnessBand(null)).toBeNull();
    expect(freshnessBand(Number.NaN)).toBeNull();
  });

  it('clamps values outside 0–1 instead of falling through', () => {
    expect(freshnessBand(2)).toBe('fresh');
    expect(freshnessBand(-1)).toBe('archiving');
  });

  it('keeps the band table ordered freshest to faintest, bottoming out at 0', () => {
    expect(FRESHNESS_BANDS.map((b) => b.id)).toEqual([...FRESHNESS_IDS]);
    const mins = FRESHNESS_BANDS.map((b) => b.min);
    expect([...mins].sort((a, b) => b - a)).toEqual(mins);
    expect(mins.at(-1)).toBe(0);
  });

  it('gives every state a style, a label key and a hint key', () => {
    for (const id of FRESHNESS_IDS) {
      expect(FRESHNESS_STYLES[id]).toBeDefined();
      expect(freshnessLabelId(id)).toBe(`memory.freshness.${id}`);
      expect(freshnessHintId(id)).toBe(`memory.freshness.${id}.hint`);
    }
  });
});

describe('the forgetting curve', () => {
  it('starts at full strength and only ever falls', () => {
    expect(retrievabilityAt(0, 14)).toBe(1);
    let previous = 1;
    for (let day = 1; day <= 60; day += 1) {
      const value = retrievabilityAt(day, 14);
      expect(value).toBeLessThan(previous);
      previous = value;
    }
    expect(previous).toBeGreaterThan(0);
  });

  it('has fallen to about a third after one stability period', () => {
    // The definition of stability: R = e⁻¹ at t = S. This is what the plain
    // wording "how many days of silence it takes to fade" is describing.
    expect(retrievabilityAt(14, 14)).toBeCloseTo(Math.E ** -1, 6);
  });

  it('lasts longer the more stable the memory is', () => {
    expect(retrievabilityAt(30, 60)).toBeGreaterThan(retrievabilityAt(30, 14));
  });

  it('round-trips against daysToReach', () => {
    const day = daysToReach(DEFAULT_ARCHIVE_THRESHOLD, 14);
    expect(day).toBeGreaterThan(0);
    expect(retrievabilityAt(day, 14)).toBeCloseTo(DEFAULT_ARCHIVE_THRESHOLD, 6);
  });

  it('degrades to zero rather than NaN on a missing stability', () => {
    expect(retrievabilityAt(5, 0)).toBe(0);
    expect(retrievabilityAt(5, Number.NaN)).toBe(0);
    expect(daysToReach(0.05, 0)).toBe(0);
    expect(daysToReach(0, 14)).toBe(0);
  });
});

describe('daysSinceRecall', () => {
  const now = Date.parse('2026-08-12T00:00:00Z');

  it('measures from the last recall when there is one', () => {
    expect(
      daysSinceRecall(
        { timestamp: '2026-07-13T00:00:00Z', last_accessed: '2026-08-10T00:00:00Z' },
        now,
      ),
    ).toBeCloseTo(2, 6);
  });

  it('falls back to when it was recorded — the anchor the backend uses', () => {
    expect(daysSinceRecall({ timestamp: '2026-08-05T00:00:00Z' }, now)).toBeCloseTo(7, 6);
    expect(
      daysSinceRecall({ timestamp: '2026-08-05T00:00:00Z', last_accessed: null }, now),
    ).toBeCloseTo(7, 6);
  });

  it('never goes negative or NaN', () => {
    expect(daysSinceRecall({ timestamp: '2099-01-01T00:00:00Z' }, now)).toBe(0);
    expect(daysSinceRecall({ timestamp: 'not a date' }, now)).toBe(0);
  });
});

describe('plain-language copy', () => {
  const catalogues = { en, 'zh-TW': zhTW, 'ja-JP': jaJP } as Record<
    string,
    Record<string, string>
  >;

  it('ships every freshness and decay string in all three catalogues', () => {
    const required = [
      ...FRESHNESS_IDS.flatMap((id) => [freshnessLabelId(id), freshnessHintId(id)]),
      'memory.freshness.legend',
      'memory.decay.range.label',
      'memory.decay.range.7d',
      'memory.decay.range.30d',
      'memory.decay.trend.title',
      'memory.decay.trend.empty',
      'memory.decay.trend.aria',
      'memory.decay.trend.hover',
      'memory.decay.trend.caption',
      'memory.decay.distribution.title',
      'memory.decay.fadingSoon',
      'memory.decay.mostRecalled',
      'memory.decay.truncated',
      'memory.decay.curve.title',
      'memory.decay.curve.archiveLine',
      'memory.decay.curve.now',
      'memory.decay.curve.lastRecall',
      'memory.decay.curve.archiveIn',
      'memory.decay.curve.summary',
      'memory.decay.curve.summaryArchiving',
      'memory.decay.curve.hover',
      'memory.detail.dialog.desc',
    ];
    for (const [locale, catalogue] of Object.entries(catalogues)) {
      const missing = required.filter((key) => !catalogue[key]?.trim());
      expect(missing, `${locale} is missing keys`).toEqual([]);
    }
  });

  it('keeps the internal vocabulary off the surface', () => {
    // DESIGN.md §5.2: users see states, never the machinery behind them.
    const banned = /retrievability|ebbinghaus|stability|decay|episodic|semantic/i;
    for (const [locale, catalogue] of Object.entries(catalogues)) {
      const leaked = Object.entries(catalogue)
        .filter(([key]) => key.startsWith('memory.freshness.') || key.startsWith('memory.decay.'))
        .filter(([, value]) => banned.test(value))
        .map(([key]) => key);
      expect(leaked, `${locale} leaks internal terms`).toEqual([]);
    }
  });

  it('tells the user the one thing they can do about fading', () => {
    // Every state's explanation ends on the same actionable fact: recalling a
    // memory is what makes it last. Without it the badge is a diagnosis with
    // no treatment.
    for (const id of FRESHNESS_IDS) {
      expect(catalogues['zh-TW'][freshnessHintId(id)]).toContain('被回想會讓記憶更持久');
      expect(catalogues.en[freshnessHintId(id)]).toContain('Using it again makes it last longer');
    }
  });
});
