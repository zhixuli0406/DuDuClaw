import { describe, it, expect } from 'vitest';
import type { BuiltinToolEntry } from '@/lib/api';
import {
  changedLists,
  computeNextLists,
  entryMatches,
  rowState,
  summarize,
  type Desired,
  type ToolLists,
} from './toolPermissionModel';

const mcp = (name: string, category = 'office'): BuiltinToolEntry => ({
  name,
  qualified: `mcp__duduclaw__${name}`,
  description: `${name} desc`,
  scope: '',
  category,
  kind: 'mcp',
});
const READ = mcp('read_file');
const WRITE = mcp('write_file');
const SEND = mcp('send_message', 'channel');
const BASH: BuiltinToolEntry = { name: 'Bash', qualified: 'Bash', description: 'shell', scope: '', category: 'claude', kind: 'claude' };
const TOOLS = [READ, WRITE, SEND, BASH];

const lists = (p: Partial<ToolLists> = {}): ToolLists => ({ allowed: [], denied: [], approval: [], ...p });
const want = (entries: Array<[BuiltinToolEntry, Partial<Desired>]>, l: ToolLists) =>
  new Map(entries.map(([t, d]) => [t.qualified, { ...rowState(t, l), ...d }]));

describe('entryMatches (mirror of tool_entry_matches)', () => {
  it('matches exact, qualified, prefix wildcard and catch-all; never another server', () => {
    expect(entryMatches('read_file', 'mcp__duduclaw__read_file')).toBe(true);
    expect(entryMatches('mcp__duduclaw__*', 'read_file')).toBe(true);
    expect(entryMatches('read_*', 'mcp__duduclaw__read_file')).toBe(true);
    expect(entryMatches('read_*', 'write_file')).toBe(false);
    expect(entryMatches('*', 'anything')).toBe(true);
    expect(entryMatches('mcp__other__read_file', 'read_file')).toBe(false);
  });
});

describe('rowState', () => {
  it('everything is allowed with empty lists', () => {
    for (const t of TOOLS) expect(rowState(t, lists())).toMatchObject({ allowed: true, approval: false });
  });

  it('denied wins over allowed; an exact deny is not locked, a wildcard deny is', () => {
    const l = lists({ allowed: ['mcp__duduclaw__*'], denied: ['write_file'] });
    expect(rowState(WRITE, l)).toMatchObject({ allowed: false, allowLocked: false });
    expect(rowState(READ, l).allowed).toBe(true);
    const w = lists({ denied: ['mcp__duduclaw__*'] });
    expect(rowState(READ, w)).toMatchObject({ allowed: false, allowLocked: true });
  });

  it('a wildcard allowlist covers MCP tools but not native Claude tools', () => {
    const l = lists({ allowed: ['mcp__duduclaw__*'] });
    expect(rowState(READ, l).allowed).toBe(true);
    expect(rowState(BASH, l).allowed).toBe(false);
    expect(rowState(BASH, lists({ allowed: ['Bash(git:*)'] })).allowed).toBe(true);
  });

  it('approval comes from the bare-name list, a wildcard entry or an ask rule (locked)', () => {
    expect(rowState(SEND, lists({ approval: ['send_message'] }))).toMatchObject({ approval: true, approvalLocked: false });
    expect(rowState(SEND, lists({ approval: ['send_*'] }))).toMatchObject({ approval: true, approvalLocked: true });
    const rules = [{ tool: 'send_message', verdict: 'ask' as const }];
    expect(rowState(SEND, lists({ actionRules: rules }))).toMatchObject({ approval: true, approvalLocked: true });
    expect(rowState(SEND, lists({ actionRules: [{ tool: 'send_message', verdict: 'block' as const }] })).approval).toBe(false);
  });
});

describe('computeNextLists', () => {
  it('unticking under a wildcard allowlist adds to denied_tools and leaves allowed_tools untouched', () => {
    const l = lists({ allowed: ['mcp__duduclaw__*'], denied: ['custom__x'] });
    const next = computeNextLists(TOOLS, l, want([[WRITE, { allowed: false }]], l));
    expect(next.allowed_tools).toEqual(['mcp__duduclaw__*']);
    expect(next.denied_tools).toEqual(['custom__x', 'mcp__duduclaw__write_file']);
    expect(changedLists(l, next)).toEqual({ denied_tools: ['custom__x', 'mcp__duduclaw__write_file'] });
  });

  it('re-ticking removes the exact deny entry (bare or qualified) and round-trips', () => {
    const l = lists({ allowed: ['mcp__duduclaw__*'], denied: ['write_file', 'x'] });
    const next = computeNextLists(TOOLS, l, want([[WRITE, { allowed: true }]], l));
    expect(next.denied_tools).toEqual(['x']);
    // off then on again → identical lists
    const l0 = lists({ allowed: ['mcp__duduclaw__*'] });
    const off = computeNextLists(TOOLS, l0, want([[READ, { allowed: false }]], l0));
    const l1 = lists({ allowed: off.allowed_tools, denied: off.denied_tools, approval: off.approval_required_tools });
    const on = computeNextLists(TOOLS, l1, want([[READ, { allowed: true }]], l1));
    expect(on).toEqual({ allowed_tools: ['mcp__duduclaw__*'], denied_tools: [], approval_required_tools: [] });
  });

  it('never expands or drops a wildcard deny: the row is locked, nothing changes', () => {
    const l = lists({ denied: ['mcp__duduclaw__*'] });
    const d = want([[READ, { allowed: true }]], l);
    expect(summarize(TOOLS, l, d).items).toEqual([]);
    expect(changedLists(l, computeNextLists(TOOLS, l, d))).toEqual({});
  });

  it.each(['Bash(git:*)', 'Bash(rm:*)'])(
    'qualified deny entry %s is never removed by re-ticking 可使用: row locked, list untouched',
    (entry) => {
      const l = lists({ denied: [entry, 'mcp__duduclaw__read_file'] });
      expect(rowState(BASH, l)).toMatchObject({ allowed: false, allowLocked: true });
      const d = want([[BASH, { allowed: true }]], l);
      expect(summarize(TOOLS, l, d).items).toEqual([]);
      expect(changedLists(l, computeNextLists(TOOLS, l, d))).toEqual({});
    },
  );

  it.each(['Bash(git:*)', 'Bash(rm:*)'])(
    'qualified approval entry %s is never removed by unticking 需審批: row locked, list untouched',
    (entry) => {
      const l = lists({ approval: [entry] });
      expect(rowState(BASH, l)).toMatchObject({ approval: true, approvalLocked: true });
      const d = want([[BASH, { approval: false }]], l);
      expect(summarize(TOOLS, l, d).items).toEqual([]);
      expect(changedLists(l, computeNextLists(TOOLS, l, d))).toEqual({});
    },
  );

  it('a bare entry beside a qualified one: the row stays locked and the qualified entry survives', () => {
    const l = lists({ denied: ['Bash', 'Bash(git:*)'] });
    expect(rowState(BASH, l).allowLocked).toBe(true);
    const d = want([[BASH, { allowed: true }]], l);
    expect(computeNextLists(TOOLS, l, d).denied_tools).toContain('Bash(git:*)');
  });

  it('ticking on a tool the (non-wildcard) allowlist does not cover appends it to allowed_tools', () => {
    const l = lists({ allowed: ['read_file'] });
    const next = computeNextLists(TOOLS, l, want([[WRITE, { allowed: true }]], l));
    expect(next.allowed_tools).toEqual(['read_file', 'mcp__duduclaw__write_file']);
    expect(next.denied_tools).toEqual([]);
  });

  it('approval: removing an exact entry leaves unrelated wildcard entries alone', () => {
    const l = lists({ approval: ['shared_*', 'send_message'] });
    expect(computeNextLists(TOOLS, l, want([[SEND, { approval: false }]], l)).approval_required_tools)
      .toEqual(['shared_*']);
  });

  it('approval on/off for plain entries', () => {
    const l = lists({ approval: ['send_message'] });
    expect(computeNextLists(TOOLS, l, want([[SEND, { approval: false }]], l)).approval_required_tools).toEqual([]);
    const l2 = lists();
    expect(computeNextLists(TOOLS, l2, want([[WRITE, { approval: true }]], l2)).approval_required_tools).toEqual(['write_file']);
    const wild = lists({ approval: ['send_*'] });
    expect(changedLists(wild, computeNextLists(TOOLS, wild, want([[SEND, { approval: false }]], wild)))).toEqual({});
  });

  it('only touched rows change; entries for tools outside the catalog pass through in order', () => {
    const l = lists({ denied: ['mcp__other__thing', 'Bash'], approval: ['mystery'] });
    const next = computeNextLists(TOOLS, l, want([[SEND, { allowed: false }]], l));
    expect(next.denied_tools).toEqual(['mcp__other__thing', 'Bash', 'mcp__duduclaw__send_message']);
    expect(next.approval_required_tools).toEqual(['mystery']);
  });
});

describe('summarize', () => {
  it('counts newly allowed, removed and approval changes', () => {
    const l = lists({ denied: ['read_file'], approval: ['send_message'] });
    const d = want(
      [
        [READ, { allowed: true }],
        [WRITE, { allowed: false }],
        [SEND, { approval: false }],
        [BASH, { approval: true }],
      ],
      l,
    );
    const s = summarize(TOOLS, l, d);
    expect(s).toMatchObject({ addedAllowed: 1, removedAllowed: 1, approvalChanged: 2 });
    expect(s.items).toHaveLength(4);
  });
});
