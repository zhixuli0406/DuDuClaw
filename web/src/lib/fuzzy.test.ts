import { describe, it, expect } from 'vitest';
import { fuzzyMatch, highlightSegments } from './fuzzy';

describe('fuzzyMatch', () => {
  it('returns score 0 with no indices for an empty query', () => {
    expect(fuzzyMatch('', 'Settings')).toEqual({ score: 0, indices: [] });
    expect(fuzzyMatch('   ', 'Settings')).toEqual({ score: 0, indices: [] });
  });

  it('returns null when the query is not a subsequence', () => {
    expect(fuzzyMatch('xyz', 'Settings')).toBeNull();
    expect(fuzzyMatch('szz', 'Settings')).toBeNull();
  });

  it('matches case-insensitively', () => {
    expect(fuzzyMatch('SET', 'settings')).not.toBeNull();
    expect(fuzzyMatch('set', 'SETTINGS')).not.toBeNull();
  });

  it('ranks an exact match highest', () => {
    const exact = fuzzyMatch('agents', 'agents')!;
    const prefix = fuzzyMatch('agent', 'agents')!;
    expect(exact.score).toBeGreaterThan(prefix.score);
  });

  it('ranks a prefix above a mid-string substring', () => {
    const prefix = fuzzyMatch('set', 'settings')!;
    const mid = fuzzyMatch('ting', 'settings')!;
    expect(prefix.score).toBeGreaterThan(mid.score);
  });

  it('returns contiguous indices for a substring hit', () => {
    expect(fuzzyMatch('gen', 'agents')!.indices).toEqual([1, 2, 3]);
  });

  it('scores a word-boundary subsequence above a scattered one', () => {
    // "km" hits the start of both words in "knowledge market".
    const boundary = fuzzyMatch('km', 'knowledge market')!;
    const scattered = fuzzyMatch('km', 'knowledgemarket')!;
    expect(boundary.score).toBeGreaterThan(scattered.score);
  });

  it('matches CJK labels by character', () => {
    const r = fuzzyMatch('設定', '系統設定')!;
    expect(r).not.toBeNull();
    expect(r.indices).toEqual([2, 3]);
  });

  it('prefers earlier matches over later ones', () => {
    const early = fuzzyMatch('a', 'agents')!;
    const late = fuzzyMatch('a', 'zzza')!;
    expect(early.score).toBeGreaterThan(late.score);
  });
});

describe('highlightSegments', () => {
  it('returns a single non-hit segment when there are no indices', () => {
    expect(highlightSegments('Settings', [])).toEqual([{ text: 'Settings', hit: false }]);
  });

  it('splits a label into hit and non-hit runs', () => {
    // indices [1,2,3] over "agents" → a | gen | ts
    expect(highlightSegments('agents', [1, 2, 3])).toEqual([
      { text: 'a', hit: false },
      { text: 'gen', hit: true },
      { text: 'ts', hit: false },
    ]);
  });

  it('handles a hit at index 0', () => {
    expect(highlightSegments('agents', [0])).toEqual([
      { text: 'a', hit: true },
      { text: 'gents', hit: false },
    ]);
  });

  it('is codepoint-safe for CJK', () => {
    expect(highlightSegments('系統設定', [2, 3])).toEqual([
      { text: '系統', hit: false },
      { text: '設定', hit: true },
    ]);
  });
});
