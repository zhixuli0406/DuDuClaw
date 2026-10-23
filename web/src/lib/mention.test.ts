import { describe, expect, it } from 'vitest';
import {
  applyMention,
  assignableAgents,
  isAssignCommand,
  filterMentionCandidates,
  findActiveMention,
  mentionErrorText,
  mentionText,
  groupMentionCandidates,
  orderMentionCandidates,
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

describe('mentionErrorText', () => {
  it('words the refusal and the choices in the viewer language', () => {
    expect(mentionErrorText({ code: 'mention_forbidden' }, 'en')).toMatch(/permission/);
    expect(mentionErrorText({ code: 'mention_forbidden' }, 'zh-TW')).toMatch(/權限/);
    const text = mentionErrorText({ code: 'mention_choices', choices: ['@財務小幫手', '@業務專員'] }, 'ja-JP');
    expect(text).toContain('• @財務小幫手');
    expect(text).toContain('• @業務專員');
    expect(mentionErrorText({ code: 'agent_forbidden' }, 'en')).toBeNull();
    expect(mentionErrorText({}, 'en')).toBeNull();
  });
});

describe('team grouping (agents.callable)', () => {
  const callable: MentionAgent[] = [
    { name: 'dudu', display_name: '嘟嘟總機', via: 'default', status: 'active' },
    { name: 'faq', display_name: '常見問題', via: 'public', status: 'active' },
    { name: 'sales-rep', display_name: '業務專員', via: 'team', via_user: 'u1', via_user_name: '王小明', status: 'active' },
    { name: 'finance', display_name: '財務小幫手', via: 'binding', status: 'active' },
    { name: 'hr', display_name: '人資助理', via: 'team', via_user: 'u2', status: 'active' },
  ];

  it('groups mine, then each report, then public, keeping order inside a group', () => {
    const groups = groupMentionCandidates(callable);
    expect(groups.map((g) => [g.kind, g.owner ?? '', g.agents.map((a) => a.name)])).toEqual([
      ['mine', '', ['dudu', 'finance']],
      ['team', '王小明', ['sales-rep']],
      ['team', 'u2', ['hr']],
      ['public', '', ['faq']],
    ]);
    expect(orderMentionCandidates(callable).map((a) => a.name)).toEqual(['dudu', 'finance', 'sales-rep', 'hr', 'faq']);
  });

  it('an agents.list result (no via) stays one group', () => {
    expect(groupMentionCandidates(agents.slice(0, 3)).map((g) => g.kind)).toEqual(['mine']);
  });

  it("prefers the server's call text", () => {
    const twin: MentionAgent = { name: 'sales-2', display_name: '業務專員', call_text: '@sales-2' };
    expect(mentionText(twin, [twin])).toBe('@sales-2');
    expect(mentionText({ name: 'x', display_name: 'X', call_text: '@' }, [])).toBe('@X');
  });
});

describe('assign command filter', () => {
  it('recognises the assign command spellings only at the start', () => {
    for (const t of ['/交辦 @', '／交辦 @業', '/assign @x', '/ASSIGN', '  /交辦']) expect(isAssignCommand(t)).toBe(true);
    for (const t of ['/交辦事項 @', '/assignment', '請 /交辦 @x', '@業務']) expect(isAssignCommand(t)).toBe(false);
  });
  it('keeps only assignable staff', () => {
    const list = [
      { name: 'a', display_name: 'A', assignable: true },
      { name: 'b', display_name: 'B', assignable: false },
      { name: 'c', display_name: 'C' },
    ];
    expect(assignableAgents(list).map((x) => x.name)).toEqual(['a']);
  });
});
