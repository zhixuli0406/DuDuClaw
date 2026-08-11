import { describe, it, expect, beforeEach } from 'vitest';
import {
  sortInbox,
  filterByTab,
  filterByCategory,
  filterByStatus,
  distinctStatuses,
  excludeArchived,
  excludeProcessed,
  sinkProcessed,
  statusLabelKey,
  ACTION_QUEUE_TABS,
  blockedBucket,
  groupKeyOf,
  withId,
  withoutId,
  loadIdSet,
  persistIdSet,
  loadPrefs,
  persistPrefs,
  expiryState,
  DEFAULT_PREFS,
  RECENT_WINDOW_MS,
  READ_KEY,
  TYPE_URGENCY,
  type InboxItem,
} from './inbox-model';

const mk = (over: Partial<InboxItem>): InboxItem => ({
  id: 'x',
  type: 'approval',
  title: 't',
  urgency: TYPE_URGENCY.approval,
  actionable: true,
  ...over,
});

describe('inbox-model sorting (§5.2)', () => {
  it('urgency sort ranks budget > failed_run > blocked > approval > decision', () => {
    const items = [
      mk({ id: 'd', type: 'decision', urgency: TYPE_URGENCY.decision }),
      mk({ id: 'a', type: 'approval', urgency: TYPE_URGENCY.approval }),
      mk({ id: 'b', type: 'budget', urgency: TYPE_URGENCY.budget }),
      mk({ id: 'f', type: 'failed_run', urgency: TYPE_URGENCY.failed_run }),
      mk({ id: 'k', type: 'blocked', urgency: TYPE_URGENCY.blocked }),
    ];
    expect(sortInbox(items, 'urgency').map((i) => i.id)).toEqual(['b', 'f', 'k', 'a', 'd']);
  });

  it('time sort ranks newest first, undated last', () => {
    const items = [
      mk({ id: 'old', timestamp: '2020-01-01T00:00:00Z' }),
      mk({ id: 'none' }),
      mk({ id: 'new', timestamp: '2026-01-01T00:00:00Z' }),
    ];
    expect(sortInbox(items, 'time').map((i) => i.id)).toEqual(['new', 'old', 'none']);
  });

  it('stuck sort ranks oldest first, undated last', () => {
    const items = [
      mk({ id: 'new', timestamp: '2026-01-01T00:00:00Z' }),
      mk({ id: 'none' }),
      mk({ id: 'old', timestamp: '2020-01-01T00:00:00Z' }),
    ];
    expect(sortInbox(items, 'stuck').map((i) => i.id)).toEqual(['old', 'new', 'none']);
  });

  it('does not mutate the input array', () => {
    const items = [mk({ id: '1' }), mk({ id: '2' })];
    const copy = [...items];
    sortInbox(items, 'urgency');
    expect(items).toEqual(copy);
  });
});

describe('inbox-model tab filtering', () => {
  const now = Date.parse('2026-07-10T00:00:00Z');
  const items: InboxItem[] = [
    mk({ id: 'act', actionable: true, timestamp: new Date(now - 1000).toISOString() }),
    mk({ id: 'passive', type: 'budget', actionable: false, timestamp: new Date(now - 1000).toISOString() }),
    mk({ id: 'old', actionable: true, timestamp: new Date(now - RECENT_WINDOW_MS - 1000).toISOString() }),
  ];

  it('mine = actionable subset', () => {
    expect(filterByTab(items, 'mine', { readIds: new Set() }).map((i) => i.id)).toEqual(['act', 'old']);
  });

  it('recent = within the 7-day window', () => {
    const ids = filterByTab(items, 'recent', { readIds: new Set(), nowMs: now }).map((i) => i.id);
    expect(ids).toEqual(['act', 'passive']);
  });

  it('unread = ids not in the read set', () => {
    const ids = filterByTab(items, 'unread', { readIds: new Set(['act']) }).map((i) => i.id);
    expect(ids).toEqual(['passive', 'old']);
  });

  it('all = everything (copy, not the same ref)', () => {
    const out = filterByTab(items, 'all', { readIds: new Set() });
    expect(out).toHaveLength(3);
    expect(out).not.toBe(items);
  });
});

describe('inbox-model blocked tri-bucket', () => {
  it('approvals & decisions → decide', () => {
    expect(blockedBucket(mk({ type: 'approval' }))).toBe('decide');
    expect(blockedBucket(mk({ type: 'decision' }))).toBe('decide');
  });
  it('assigned blocked task → input, unassigned → attention', () => {
    expect(blockedBucket(mk({ type: 'blocked', agentId: 'sam' }))).toBe('input');
    expect(blockedBucket(mk({ type: 'blocked', agentId: undefined }))).toBe('attention');
  });
  it('a needs_human task → decide, even when assigned (it is a decision, not an input request)', () => {
    expect(blockedBucket(mk({ type: 'blocked', agentId: 'sam', status: 'needs_human' }))).toBe('decide');
    expect(blockedBucket(mk({ type: 'blocked', agentId: undefined, status: 'needs_human' }))).toBe('decide');
  });
  it('budget & failed_run → attention', () => {
    expect(blockedBucket(mk({ type: 'budget' }))).toBe('attention');
    expect(blockedBucket(mk({ type: 'failed_run' }))).toBe('attention');
  });
});

describe('inbox-model grouping', () => {
  it('group keys per mode', () => {
    const it = mk({ type: 'blocked', agentId: 'sam', channel: 'slack' });
    expect(groupKeyOf(it, 'none')).toBe('');
    expect(groupKeyOf(it, 'type')).toBe('blocked');
    expect(groupKeyOf(it, 'agent')).toBe('sam');
    expect(groupKeyOf(it, 'channel')).toBe('slack');
  });
  it('missing agent/channel fall back to em-dash bucket', () => {
    const it = mk({ agentId: undefined, channel: undefined });
    expect(groupKeyOf(it, 'agent')).toBe('—');
    expect(groupKeyOf(it, 'channel')).toBe('—');
  });
});

describe('inbox-model "全部" filters', () => {
  const items: InboxItem[] = [
    mk({ id: 'a', type: 'approval', status: 'pending' }),
    mk({ id: 'b', type: 'blocked', status: 'blocked' }),
    mk({ id: 'c', type: 'failed_run', status: 'critical' }),
  ];
  it('category filter', () => {
    expect(filterByCategory(items, 'all')).toHaveLength(3);
    expect(filterByCategory(items, 'blocked').map((i) => i.id)).toEqual(['b']);
  });
  it('status filter', () => {
    expect(filterByStatus(items, 'critical').map((i) => i.id)).toEqual(['c']);
  });
  it('distinctStatuses sorted', () => {
    expect(distinctStatuses(items)).toEqual(['blocked', 'critical', 'pending']);
  });
});

describe('inbox-model archive exclusion + id sets', () => {
  it('excludeArchived removes archived ids', () => {
    const items = [mk({ id: '1' }), mk({ id: '2' })];
    expect(excludeArchived(items, new Set(['1'])).map((i) => i.id)).toEqual(['2']);
    expect(excludeArchived(items, new Set())).toHaveLength(2);
  });
  it('withId / withoutId are immutable', () => {
    const base = new Set(['a']);
    const added = withId(base, 'b');
    expect([...base]).toEqual(['a']);
    expect([...added].sort()).toEqual(['a', 'b']);
    const removed = withoutId(added, 'a');
    expect([...removed]).toEqual(['b']);
  });
});

// ── "已讀" vs "處理完" (§C6) ─────────────────────────────────────────────────

describe('inbox-model processed axis (§C6)', () => {
  it('excludeProcessed drops processed ids, keeps the rest', () => {
    const items = [mk({ id: '1' }), mk({ id: '2' }), mk({ id: '3' })];
    expect(excludeProcessed(items, new Set(['2'])).map((i) => i.id)).toEqual(['1', '3']);
    expect(excludeProcessed(items, new Set())).toHaveLength(3);
    // Empty-set fast path still returns a fresh array, not the same ref.
    expect(excludeProcessed(items, new Set())).not.toBe(items);
  });

  it('sinkProcessed moves processed items to the end, preserving relative order within each partition', () => {
    const items = [mk({ id: 'a' }), mk({ id: 'b' }), mk({ id: 'c' }), mk({ id: 'd' })];
    expect(sinkProcessed(items, new Set(['b', 'd'])).map((i) => i.id)).toEqual(['a', 'c', 'b', 'd']);
  });

  it('sinkProcessed never drops an item — only reorders', () => {
    const items = [mk({ id: 'a' }), mk({ id: 'b' })];
    expect(sinkProcessed(items, new Set(['a', 'b']))).toHaveLength(2);
  });

  it('sinkProcessed with an empty set is a no-op copy', () => {
    const items = [mk({ id: 'a' }), mk({ id: 'b' })];
    const out = sinkProcessed(items, new Set());
    expect(out).toEqual(items);
    expect(out).not.toBe(items);
  });

  it('ACTION_QUEUE_TABS is exactly the actionable queues, not the time-ordered views', () => {
    expect([...ACTION_QUEUE_TABS].sort()).toEqual(['blocked', 'mine']);
  });
});

describe('inbox-model statusLabelKey (§C.2/§C.3)', () => {
  it('pending and needs_human share one message id — the object-model convergence payoff', () => {
    expect(statusLabelKey('pending')).toBe(statusLabelKey('needs_human'));
    expect(statusLabelKey('pending')).toBe('inbox.status.pending');
  });
  it('maps the install two-stage tokens', () => {
    expect(statusLabelKey('awaiting_manager')).toBe('inbox.status.awaitingManager');
    expect(statusLabelKey('awaiting_admin')).toBe('inbox.status.awaitingAdmin');
  });
  it('unmapped tokens (open-ended business values) return null — caller falls back to the raw string', () => {
    expect(statusLabelKey('critical')).toBeNull();
    expect(statusLabelKey('some_future_status')).toBeNull();
  });
});

describe('inbox-model persistence', () => {
  beforeEach(() => localStorage.clear());

  it('round-trips an id set', () => {
    persistIdSet(READ_KEY, new Set(['x', 'y']));
    expect([...loadIdSet(READ_KEY)].sort()).toEqual(['x', 'y']);
  });

  it('loadIdSet tolerates garbage', () => {
    localStorage.setItem(READ_KEY, '{not json');
    expect(loadIdSet(READ_KEY).size).toBe(0);
  });

  it('prefs round-trip and default missing fields', () => {
    persistPrefs({ ...DEFAULT_PREFS, tab: 'blocked', sortBy: 'stuck' });
    const p = loadPrefs();
    expect(p.tab).toBe('blocked');
    expect(p.sortBy).toBe('stuck');
    expect(p.columns.length).toBeGreaterThan(0);
  });

  it('loadPrefs ignores invalid enum values', () => {
    localStorage.setItem('duduclaw:inbox:prefs', JSON.stringify({ tab: 'bogus', sortBy: 'nope' }));
    const p = loadPrefs();
    expect(p.tab).toBe(DEFAULT_PREFS.tab);
    expect(p.sortBy).toBe(DEFAULT_PREFS.sortBy);
  });
});

// ── approval TTL countdown ───────────────────────────────────────────────────

describe('expiryState', () => {
  const created = '2026-01-01T00:00:00Z';
  const createdMs = Date.parse(created);

  it('is null when there is no expiresAt (every non-approval type)', () => {
    expect(expiryState({ timestamp: created, expiresAt: undefined }, createdMs)).toBeNull();
    expect(expiryState({ timestamp: created, expiresAt: null }, createdMs)).toBeNull();
  });

  it('is null when the timestamp is missing or unparseable', () => {
    const expiresAt = createdMs / 1000 + 300;
    expect(expiryState({ expiresAt }, createdMs)).toBeNull();
    expect(expiryState({ timestamp: 'not-a-date', expiresAt }, createdMs)).toBeNull();
  });

  it('is null on a non-positive window (expiresAt at/before timestamp)', () => {
    expect(expiryState({ timestamp: created, expiresAt: createdMs / 1000 }, createdMs)).toBeNull();
    expect(expiryState({ timestamp: created, expiresAt: createdMs / 1000 - 10 }, createdMs)).toBeNull();
  });

  it('reports remainingMs and nearExpiry=false well inside the window', () => {
    // 300s TTL, 10s elapsed ⇒ 290s left, nowhere near the last third.
    const expiresAt = createdMs / 1000 + 300;
    const state = expiryState({ timestamp: created, expiresAt }, createdMs + 10_000);
    expect(state).not.toBeNull();
    expect(state!.remainingMs).toBe(290_000);
    expect(state!.nearExpiry).toBe(false);
    expect(state!.expired).toBe(false);
  });

  it('flips nearExpiry once at most a third of the window remains', () => {
    // 300s TTL ⇒ last third starts at 200s elapsed (100s left).
    const expiresAt = createdMs / 1000 + 300;
    const justBefore = expiryState({ timestamp: created, expiresAt }, createdMs + 199_000);
    const atThreshold = expiryState({ timestamp: created, expiresAt }, createdMs + 200_000);
    expect(justBefore!.nearExpiry).toBe(false);
    expect(atThreshold!.nearExpiry).toBe(true);
    expect(atThreshold!.remainingMs).toBe(100_000);
  });

  it('clamps remainingMs to 0 and reports expired=true past the deadline', () => {
    const expiresAt = createdMs / 1000 + 300;
    const state = expiryState({ timestamp: created, expiresAt }, createdMs + 999_000);
    expect(state!.remainingMs).toBe(0);
    expect(state!.expired).toBe(true);
    expect(state!.nearExpiry).toBe(false);
  });
});
