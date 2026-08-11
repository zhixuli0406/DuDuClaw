import { describe, it, expect } from 'vitest';
import { looksLikeIdQuery, findIdMatch, idMatchRoute, type IdMatch } from './id-lookup';

describe('looksLikeIdQuery', () => {
  it('accepts a UUID v4 (task/approval/install id shape)', () => {
    expect(looksLikeIdQuery('3f9a1b2c-4d5e-4f60-8a12-abcdef123456')).toBe(true);
  });

  it('accepts a colon/hash-delimited conversation session id', () => {
    expect(looksLikeIdQuery('webchat:webchat:tag:conn1#agent:sales#conv:xyz')).toBe(true);
    expect(looksLikeIdQuery('telegram:123456789')).toBe(true);
  });

  it('accepts a short kebab-case agent slug', () => {
    expect(looksLikeIdQuery('sales-bot')).toBe(true);
  });

  it('rejects strings shorter than 6 characters', () => {
    expect(looksLikeIdQuery('ab')).toBe(false);
    expect(looksLikeIdQuery('abcde')).toBe(false);
  });

  it('rejects natural-language phrases (contain whitespace)', () => {
    expect(looksLikeIdQuery('open tasks')).toBe(false);
  });

  it('rejects CJK text — always a name/keyword search, never a pasted id', () => {
    expect(looksLikeIdQuery('任務看板')).toBe(false);
    expect(looksLikeIdQuery('タスクボード')).toBe(false);
  });

  it('rejects empty / whitespace-only input', () => {
    expect(looksLikeIdQuery('')).toBe(false);
    expect(looksLikeIdQuery('   ')).toBe(false);
  });

  it('rejects a leading/trailing separator (not a plausible token)', () => {
    expect(looksLikeIdQuery('-abcdef')).toBe(false);
    expect(looksLikeIdQuery('abcdef-')).toBe(false);
  });

  it('trims surrounding whitespace before judging shape', () => {
    expect(looksLikeIdQuery('  sales-bot  ')).toBe(true);
  });
});

describe('findIdMatch', () => {
  const sources = {
    tasks: [{ id: 'task-1' }, { id: 'task-2' }],
    approvals: [{ id: 'apr-1' }],
    installs: [{ id: 'inst-1' }],
    agents: [{ name: 'nova' }, { name: 'agnes' }],
    conversations: [{ session_id: 'webchat:webchat:tag:conn1' }],
  };

  it('matches a task by exact id', () => {
    expect(findIdMatch('task-2', sources)).toEqual({ kind: 'task', id: 'task-2' });
  });

  it('matches an approval by exact id', () => {
    expect(findIdMatch('apr-1', sources)).toEqual({ kind: 'approval', id: 'apr-1' });
  });

  it('matches an install request by exact id', () => {
    expect(findIdMatch('inst-1', sources)).toEqual({ kind: 'install', id: 'inst-1' });
  });

  it('matches an agent by exact name', () => {
    expect(findIdMatch('agnes', sources)).toEqual({ kind: 'agent', id: 'agnes' });
  });

  it('matches a conversation by exact session id', () => {
    expect(findIdMatch('webchat:webchat:tag:conn1', sources)).toEqual({
      kind: 'conversation',
      id: 'webchat:webchat:tag:conn1',
    });
  });

  it('returns null when nothing matches (all-miss)', () => {
    expect(findIdMatch('does-not-exist', sources)).toBeNull();
  });

  it('returns null for an empty query', () => {
    expect(findIdMatch('', sources)).toBeNull();
    expect(findIdMatch('   ', sources)).toBeNull();
  });

  it('does not require every source to be provided', () => {
    expect(findIdMatch('nova', { agents: sources.agents })).toEqual({ kind: 'agent', id: 'nova' });
    expect(findIdMatch('nova', {})).toBeNull();
  });

  it('resolves a colliding id deterministically — task wins over approval', () => {
    const colliding = {
      tasks: [{ id: 'dup-id' }],
      approvals: [{ id: 'dup-id' }],
    };
    expect(findIdMatch('dup-id', colliding)).toEqual({ kind: 'task', id: 'dup-id' });
  });

  it('resolves a colliding id deterministically — approval wins over install', () => {
    const colliding = {
      approvals: [{ id: 'dup-id' }],
      installs: [{ id: 'dup-id' }],
    };
    expect(findIdMatch('dup-id', colliding)).toEqual({ kind: 'approval', id: 'dup-id' });
  });
});

describe('idMatchRoute', () => {
  it('routes a task match to /tasks/<id>', () => {
    const m: IdMatch = { kind: 'task', id: 'task-1' };
    expect(idMatchRoute(m)).toBe('/tasks/task-1');
  });

  it('routes an approval match to /inbox?item=<id>', () => {
    const m: IdMatch = { kind: 'approval', id: 'apr-1' };
    expect(idMatchRoute(m)).toBe('/inbox?item=apr-1');
  });

  it('routes an install match to /inbox?item=<id>', () => {
    const m: IdMatch = { kind: 'install', id: 'inst-1' };
    expect(idMatchRoute(m)).toBe('/inbox?item=inst-1');
  });

  it('routes an agent match to /agents/<id>', () => {
    const m: IdMatch = { kind: 'agent', id: 'nova' };
    expect(idMatchRoute(m)).toBe('/agents/nova');
  });

  it('returns null for a conversation match (handled specially by the caller)', () => {
    const m: IdMatch = { kind: 'conversation', id: 'webchat:webchat:tag:conn1' };
    expect(idMatchRoute(m)).toBeNull();
  });

  it('URL-encodes ids that need it', () => {
    const m: IdMatch = { kind: 'task', id: 'task with space' };
    expect(idMatchRoute(m)).toBe('/tasks/task%20with%20space');
  });
});
