import { describe, it, expect } from 'vitest';
import {
  classifyMemory,
  groupByCategory,
  termMatches,
  MEMORY_CATEGORIES,
} from './memory-category';

describe('termMatches', () => {
  it('requires a word boundary for ASCII terms', () => {
    expect(termMatches('call the api later', 'api')).toBe(true);
    expect(termMatches('rapid prototyping', 'api')).toBe(false);
    expect(termMatches('is there a rule', 'hr')).toBe(false);
  });

  it('finds a later non-embedded occurrence when the first is embedded', () => {
    expect(termMatches('rapid then api', 'api')).toBe(true);
  });

  it('treats punctuation and CJK as boundaries', () => {
    expect(termMatches('use the (api).', 'api')).toBe(true);
    expect(termMatches('請走 api 流程', 'api')).toBe(true);
  });

  it('matches CJK terms as plain substrings', () => {
    expect(termMatches('這個專案下週上線', '專案')).toBe(true);
    expect(termMatches('這個專案下週上線', '報價')).toBe(false);
  });
});

describe('classifyMemory', () => {
  it('lets a known origin win over content keywords', () => {
    // Content mentions an app name and "使用時長"; without the origin rule the
    // keyword pass could file it elsewhere.
    const entry = {
      content: '2026-07-29 前景應用程式使用時長 Top5（UTC）：Code（10h45m）、Chrome（2m）',
      tags: ['footprint-distill'],
      source_event: 'footprint_distill',
    };
    expect(classifyMemory(entry)).toBe('footprint');
  });

  it('classifies consolidated reflexion rules as rules', () => {
    expect(
      classifyMemory({
        content: 'Always confirm the recipient before sending.',
        tags: ['reflexion', 'consolidated'],
        source_event: 'reflexion_consolidation',
      }),
    ).toBe('rules');
  });

  it('falls back to tags when source_event is unknown', () => {
    expect(
      classifyMemory({ content: '隨手記', tags: ['user-profile'], source_event: 'something_new' }),
    ).toBe('preferences');
  });

  it('classifies plain zh-TW content by keywords', () => {
    expect(classifyMemory({ content: '客戶的合約報價是三萬元' })).toBe('business');
    expect(classifyMemory({ content: '老闆喜歡簡短的回覆語氣' })).toBe('preferences');
    expect(classifyMemory({ content: '發票要在每月五號前報帳' })).toBe('finance');
  });

  it('classifies English content by keywords', () => {
    expect(classifyMemory({ content: 'Deploy goes through the staging server first' })).toBe(
      'tools',
    );
  });

  it('returns other when nothing matches', () => {
    expect(classifyMemory({ content: '嗯嗯' })).toBe('other');
    expect(classifyMemory({ content: 'ok' })).toBe('other');
  });

  it('is stable — repeated calls give the same bucket', () => {
    const entry = { content: '客戶的專案進度要在會議前更新' };
    const first = classifyMemory(entry);
    expect(classifyMemory(entry)).toBe(first);
  });
});

describe('groupByCategory', () => {
  it('drops empty categories and preserves entry order', () => {
    const entries = [
      { content: '客戶合約已簽約' },
      { content: '客戶提案下週交付' },
      { content: '發票要報帳' },
    ];
    const buckets = groupByCategory(entries);
    const ids = buckets.map((b) => b.category.id);
    expect(ids).not.toContain('footprint');
    expect(ids).toContain('business');
    expect(ids).toContain('finance');
    const business = buckets.find((b) => b.category.id === 'business');
    expect(business?.entries[0].content).toBe('客戶合約已簽約');
  });

  it('follows the declared category order', () => {
    const entries = [{ content: '發票要報帳' }, { content: '這個專案下週上線' }];
    const ids = groupByCategory(entries).map((b) => b.category.id);
    const order = MEMORY_CATEGORIES.map((c) => c.id);
    expect(order.indexOf(ids[0])).toBeLessThan(order.indexOf(ids[1]));
  });

  it('handles an empty list', () => {
    expect(groupByCategory([])).toEqual([]);
  });
});
