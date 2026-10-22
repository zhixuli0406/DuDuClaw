import { describe, it, expect } from 'vitest';
import {
  attachHistoryCitations,
  parseChatCitations,
  parseSessionCitationRows,
  splitCitationMarkers,
} from './knowledge-citations';
import { historyToMessages } from '@/stores/chat-store';

const good = { n: 1, chunk_id: 7, source_id: 's1', title: '員工手冊.pdf', locator: '第 3 頁', excerpt: '特休' };

describe('parseChatCitations', () => {
  it('keeps well-formed entries and drops malformed ones', () => {
    const out = parseChatCitations([good, null, 'x', { n: 'a' }, { ...good, n: 0 }, { ...good, n: 1 }, { ...good, n: 2 }]);
    expect(out?.map((c) => c.n)).toEqual([1, 2]);
    expect(out?.[0].state).toBe('available');
  });
  it('returns undefined for absent / non-array / all-bad input', () => {
    expect(parseChatCitations(undefined)).toBeUndefined();
    expect(parseChatCitations({})).toBeUndefined();
    expect(parseChatCitations([{ n: 'x' }])).toBeUndefined();
  });
  it('caps at 30', () => {
    const many = Array.from({ length: 50 }, (_, i) => ({ ...good, n: (i % 99) + 1 }));
    expect(parseChatCitations(many)?.length).toBe(30);
  });
});

describe('splitCitationMarkers', () => {
  const ns = new Set([1, 2]);
  it('turns valid markers into cite segments', () => {
    const segs = splitCitationMarkers('a [1] b [2].', ns);
    expect(segs.map((s) => s.kind)).toEqual(['text', 'cite', 'text', 'cite', 'text']);
  });
  it('leaves markers without a citation as plain text and preserves content', () => {
    const segs = splitCitationMarkers('see [3] and [1]', ns);
    expect(segs).toEqual([
      { kind: 'text', text: 'see [3] and ' },
      { kind: 'cite', n: 1, raw: '[1]' },
    ]);
  });
  it('does not transform inside code', () => {
    const segs = splitCitationMarkers('x `a[1]` y\n```\nz[2]\n```\n[1]', ns);
    expect(segs.filter((s) => s.kind === 'cite')).toHaveLength(1);
    expect(segs.map((s) => (s.kind === 'text' ? s.text : s.raw)).join('')).toBe('x `a[1]` y\n```\nz[2]\n```\n[1]');
  });
});

describe('history attach', () => {
  it('attaches rows to assistant messages by stored id', () => {
    const msgs = historyToMessages([
      { role: 'user', content: 'q', timestamp: '2026-01-01T00:00:00Z', id: 10 },
      { role: 'assistant', content: 'a [1]', timestamp: '2026-01-01T00:00:01Z', id: 11 },
      { role: 'assistant', content: 'b', timestamp: '2026-01-01T00:00:02Z' },
    ]);
    expect(msgs[1].historyId).toBe(11);
    expect(msgs[2].historyId).toBeUndefined();
    const rows = parseSessionCitationRows({
      citations: [
        { turn_id: 't', assistant_message_id: 11, n: 1, chunk_id: 7, source_id: 's', title: 'T', locator: 'L', excerpt: 'E', state: 'available' },
        { turn_id: 't', assistant_message_id: 11, n: 2, chunk_id: null, source_id: 's', title: 'Gone', locator: '', excerpt: '', state: 'deleted' },
        { turn_id: 't', assistant_message_id: null, n: 1, chunk_id: 1, source_id: 's', title: '', locator: '', excerpt: '', state: 'available' },
        { n: 1, state: 'bogus' },
      ],
    });
    expect(rows).toHaveLength(3);
    const out = attachHistoryCitations(msgs, rows);
    expect(out[1].citations?.map((c) => [c.n, c.state])).toEqual([[1, 'available'], [2, 'deleted']]);
    expect(out[0].citations).toBeUndefined();
    expect(out[2].citations).toBeUndefined();
  });
  it('tolerates garbage payloads', () => {
    expect(parseSessionCitationRows(null)).toEqual([]);
    expect(parseSessionCitationRows({ citations: 3 })).toEqual([]);
  });
});
