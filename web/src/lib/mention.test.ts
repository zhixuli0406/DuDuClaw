import { describe, expect, it } from 'vitest';
import {
  applyMention,
  filterMentionCandidates,
  findActiveMention,
  mentionText,
  type MentionAgent,
} from './mention';

const agents: MentionAgent[] = [
  { name: 'dudu', display_name: '嘟嘟總機', trigger: '@嘟嘟總機', role: 'main', status: 'active' },
  { name: 'finance', display_name: '財務小幫手', trigger: '@財務小幫手', status: 'active' },
  { name: 'sales-rep', display_name: '業務專員', trigger: '@sales-rep', status: 'active' },
  { name: 'old', display_name: '舊員工', status: 'paused' },
];

describe('findActiveMention', () => {
  it('opens on a bare @ and tracks the typed query', () => {
    expect(findActiveMention('@', 1)).toEqual({ start: 0, end: 1, query: '' });
    expect(findActiveMention('請 @財務', 5)).toEqual({ start: 2, end: 5, query: '財務' });
    expect(findActiveMention('＠sal', 4)?.query).toBe('sal');
  });

  it('closes after a space and ignores e-mail addresses', () => {
    expect(findActiveMention('@財務小幫手 幫我', 9)).toBeNull();
    expect(findActiveMention('ann@exa', 7)).toBeNull();
    expect(findActiveMention('no mention here', 5)).toBeNull();
  });

  it('opens after CJK text glued to the @', () => {
    expect(findActiveMention('請@財', 3)?.query).toBe('財');
  });
});

describe('filterMentionCandidates', () => {
  it('lists every active staff member for an empty query', () => {
    expect(filterMentionCandidates(agents, '').map((a) => a.name)).toEqual(['dudu', 'finance', 'sales-rep']);
  });

  it('filters by display name, trigger or id, case-insensitively', () => {
    expect(filterMentionCandidates(agents, '財務').map((a) => a.name)).toEqual(['finance']);
    expect(filterMentionCandidates(agents, 'SAL').map((a) => a.name)).toEqual(['sales-rep']);
    expect(filterMentionCandidates(agents, '專員').map((a) => a.name)).toEqual(['sales-rep']);
    expect(filterMentionCandidates(agents, '舊')).toEqual([]);
  });
});

describe('mentionText / applyMention', () => {
  it('inserts the display name, or the id when the name is shared', () => {
    expect(mentionText(agents[1], agents)).toBe('@財務小幫手');
    const dup: MentionAgent[] = [
      { name: 'a1', display_name: '助理' },
      { name: 'a2', display_name: '助理' },
    ];
    expect(mentionText(dup[1], dup)).toBe('@a2');
  });

  it('replaces the typed fragment and places the caret after a space', () => {
    const text = '請 @財 幫我';
    const m = findActiveMention(text, 4)!;
    const out = applyMention(text, m, '@財務小幫手');
    expect(out.text).toBe('請 @財務小幫手 幫我');
    expect(out.caret).toBe('請 @財務小幫手 '.length);
  });
});
