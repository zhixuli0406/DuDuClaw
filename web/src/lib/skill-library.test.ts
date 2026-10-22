import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  sortSkills,
  matchesQuery,
  sortAgentsByName,
  readSortPreference,
  writeSortPreference,
  parseSkillTab,
  type SortFields,
} from './skill-library';

interface Row {
  name: string;
  owner: string;
  last?: string | null;
  uses?: number;
  added?: number | null;
}

const fields = (r: Row): SortFields => ({
  name: r.name,
  lastUsedAt: r.last ?? null,
  useCount: r.uses ?? 0,
  addedAt: r.added ?? null,
  stableKey: `${r.owner}:${r.name}`,
});

const names = (rows: Row[]) => rows.map((r) => r.name);

describe('sortSkills', () => {
  it('orders mixed case alphabetically instead of uppercase-first byte order', () => {
    const rows: Row[] = ['beta', 'Zulu', 'alpha', 'Charlie'].map((name) => ({ name, owner: 'a' }));
    expect(names(sortSkills(rows, 'name', 'en', fields))).toEqual(['alpha', 'beta', 'Charlie', 'Zulu']);
  });

  it('compares numbers numerically', () => {
    const rows: Row[] = ['step-10', 'step-2', 'step-1'].map((name) => ({ name, owner: 'a' }));
    expect(names(sortSkills(rows, 'name', 'en', fields))).toEqual(['step-1', 'step-2', 'step-10']);
  });

  it('uses the locale collator for CJK and keeps the order stable across calls', () => {
    const rows: Row[] = ['報價', '客服', '出貨', 'pdf', '會計'].map((name) => ({ name, owner: 'a' }));
    const expected = [...rows.map((r) => r.name)].sort(new Intl.Collator('zh-TW', { numeric: true, sensitivity: 'base' }).compare);
    const once = names(sortSkills(rows, 'name', 'zh-TW', fields));
    expect(once).toEqual(expected);
    expect(names(sortSkills([...rows].reverse(), 'name', 'zh-TW', fields))).toEqual(once);
  });

  it('breaks ties between same-named skills of two employees by a stable key', () => {
    const rows: Row[] = [
      { name: 'quote', owner: 'bob' },
      { name: 'quote', owner: 'alice' },
    ];
    const a = sortSkills(rows, 'name', 'en', fields).map((r) => r.owner);
    const b = sortSkills([...rows].reverse(), 'name', 'en', fields).map((r) => r.owner);
    expect(a).toEqual(['alice', 'bob']);
    expect(b).toEqual(a);
  });

  it('sorts by recent use, most use and recently added, unknowns last', () => {
    const rows: Row[] = [
      { name: 'a', owner: 'x', last: '2026-10-01T00:00:00Z', uses: 2, added: 100 },
      { name: 'b', owner: 'x', last: null, uses: 9, added: null },
      { name: 'c', owner: 'x', last: '2026-10-05T00:00:00Z', uses: 2, added: 300 },
    ];
    expect(names(sortSkills(rows, 'recentlyUsed', 'en', fields))).toEqual(['c', 'a', 'b']);
    expect(names(sortSkills(rows, 'mostUsed', 'en', fields))).toEqual(['b', 'a', 'c']);
    expect(names(sortSkills(rows, 'recentlyAdded', 'en', fields))).toEqual(['c', 'a', 'b']);
  });

  it('does not modify its input', () => {
    const rows: Row[] = [{ name: 'b', owner: 'x' }, { name: 'a', owner: 'x' }];
    sortSkills(rows, 'name', 'en', fields);
    expect(names(rows)).toEqual(['b', 'a']);
  });
});

describe('matchesQuery', () => {
  it('matches name, description or tags, case-insensitively, every term', () => {
    const f = ['invoice-ocr', '讀取發票並整理金額', 'Finance', 'ocr'];
    expect(matchesQuery('', f)).toBe(true);
    expect(matchesQuery('INVOICE', f)).toBe(true);
    expect(matchesQuery('發票', f)).toBe(true);
    expect(matchesQuery('finance ocr', f)).toBe(true);
    expect(matchesQuery('finance payroll', f)).toBe(false);
  });

  it('folds full-width characters', () => {
    expect(matchesQuery('ＰＤＦ', ['pdf'])).toBe(true);
  });
});

describe('sortAgentsByName', () => {
  it('orders by display name with the collator, falling back to the id', () => {
    const agents = [
      { name: 'z1', display_name: 'amy' },
      { name: 'a1', display_name: 'Zed' },
      { name: 'm1', display_name: '' },
    ];
    expect(sortAgentsByName(agents, 'en').map((a) => a.name)).toEqual(['z1', 'm1', 'a1']);
  });
});

describe('sort preference storage', () => {
  beforeEach(() => {
    try {
      localStorage.clear();
    } catch {
      /* jsdom */
    }
    vi.restoreAllMocks();
  });

  it('round-trips and ignores unknown values', () => {
    expect(readSortPreference('installed', 'name')).toBe('name');
    writeSortPreference('installed', 'mostUsed');
    expect(readSortPreference('installed', 'name')).toBe('mostUsed');
    localStorage.setItem('duduclaw.skills.sort.installed', 'bogus');
    expect(readSortPreference('installed', 'name')).toBe('name');
  });

  it('falls back when storage throws', () => {
    vi.spyOn(window.localStorage, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    vi.spyOn(window.localStorage, 'setItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    expect(() => writeSortPreference('installed', 'mostUsed')).not.toThrow();
    expect(readSortPreference('installed', 'recentlyUsed')).toBe('recentlyUsed');
  });
});

describe('parseSkillTab', () => {
  it('accepts known tabs and defaults to the market', () => {
    expect(parseSkillTab('shared')).toBe('shared');
    expect(parseSkillTab('mySkills')).toBe('mySkills');
    expect(parseSkillTab('nope')).toBe('market');
    expect(parseSkillTab(null)).toBe('market');
  });
});
