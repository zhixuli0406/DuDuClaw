import { describe, it, expect } from 'vitest';
import {
  countStandbyAgents,
  withinRecentWindow,
  buildAttentionItems,
  overviewCounts,
  isAllClear,
  channelFailureChannel,
  isChannelRecoveredEvent,
  excludeRecoveredChannelFailures,
  UNDELIVERED_WINDOW_MS,
  type AttentionSourceRow,
  type ChannelFailureAuditEvent,
} from './home-overview';

describe('countStandbyAgents', () => {
  it('counts only active, non-archived agents', () => {
    const agents = [
      { status: 'active' },
      { status: 'active', archived: false },
      { status: 'paused' },
      { status: 'terminated' },
      { status: 'active', archived: true },
    ];
    expect(countStandbyAgents(agents)).toBe(2);
  });

  it('empty roster → 0, honestly (not a fallback narrative)', () => {
    expect(countStandbyAgents([])).toBe(0);
  });
});

describe('withinRecentWindow', () => {
  const now = Date.parse('2026-08-11T12:00:00Z');

  it('keeps items inside the window and drops items outside it', () => {
    const items = [
      { id: 'inside', timestamp: new Date(now - 1000).toISOString() },
      { id: 'boundary', timestamp: new Date(now - UNDELIVERED_WINDOW_MS).toISOString() },
      { id: 'outside', timestamp: new Date(now - UNDELIVERED_WINDOW_MS - 1000).toISOString() },
    ];
    const kept = withinRecentWindow(items, now, UNDELIVERED_WINDOW_MS).map((i) => i.id);
    expect(kept).toEqual(['inside', 'boundary']);
  });

  it('drops unparseable and future timestamps rather than guessing', () => {
    const items = [
      { id: 'garbage', timestamp: 'not-a-date' },
      { id: 'future', timestamp: new Date(now + 60_000).toISOString() },
    ];
    expect(withinRecentWindow(items, now, UNDELIVERED_WINDOW_MS)).toEqual([]);
  });
});

describe('buildAttentionItems', () => {
  const approvals: AttentionSourceRow[] = [
    { id: 'a1', title: 'Approve refund', agentId: 'sam', timestamp: '2026-08-11T10:00:00Z' },
  ];
  const needsHuman: AttentionSourceRow[] = [
    { id: 't1', title: 'Goal stuck', agentId: 'nova', timestamp: '2026-08-11T09:00:00Z' },
  ];
  const undelivered: AttentionSourceRow[] = [
    { id: 'f1', title: 'Telegram send failed', agentId: 'nova', timestamp: '2026-08-11T11:00:00Z' },
  ];

  it('tags each source with its InboxItem type and a prefixed id', () => {
    const items = buildAttentionItems(approvals, needsHuman, undelivered);
    const byType = Object.fromEntries(items.map((i) => [i.type, i]));
    expect(byType.approval).toMatchObject({ id: 'approval:a1', title: 'Approve refund', actionable: true });
    expect(byType.blocked).toMatchObject({ id: 'blocked:t1', status: 'needs_human', actionable: true });
    expect(byType.failed_run).toMatchObject({ id: 'failed_run:f1', actionable: false });
  });

  it('sorts by urgency (budget > failed_run > blocked > approval > decision)', () => {
    const items = buildAttentionItems(approvals, needsHuman, undelivered);
    expect(items.map((i) => i.type)).toEqual(['failed_run', 'blocked', 'approval']);
  });

  it('empty sources → empty list, not a placeholder row', () => {
    expect(buildAttentionItems([], [], [])).toEqual([]);
  });
});

// ── W2-8: channel_recovered handling ────────────────────────────────────

function failureEvent(overrides: Partial<ChannelFailureAuditEvent> = {}): ChannelFailureAuditEvent {
  return {
    timestamp: '2026-08-11T09:00:00Z',
    event_type: 'channel.telegram_send_failed',
    agent_id: 'nova',
    summary: '401 Unauthorized',
    details: { channel_failure: { channel: 'telegram' } },
    ...overrides,
  };
}

function recoveredEvent(overrides: Partial<ChannelFailureAuditEvent> = {}): ChannelFailureAuditEvent {
  return {
    timestamp: '2026-08-11T09:30:00Z',
    event_type: 'channel.recovered',
    agent_id: '',
    summary: '通道「Telegram」已恢復正常發送',
    details: { channel_failure: { channel: 'telegram' } },
    ...overrides,
  };
}

describe('channelFailureChannel', () => {
  it('reads the channel from the nested details.channel_failure.channel path', () => {
    expect(channelFailureChannel(failureEvent())).toBe('telegram');
  });

  it('returns undefined for the flatter (wrong) details.channel path, empty, or missing details', () => {
    expect(channelFailureChannel({ ...failureEvent(), details: { channel: 'telegram' } })).toBeUndefined();
    expect(channelFailureChannel({ ...failureEvent(), details: { channel_failure: { channel: '' } } })).toBeUndefined();
    expect(channelFailureChannel({ ...failureEvent(), details: undefined })).toBeUndefined();
    expect(channelFailureChannel({ ...failureEvent(), details: {} })).toBeUndefined();
  });
});

describe('isChannelRecoveredEvent', () => {
  it('matches only the exact channel.recovered event_type', () => {
    expect(isChannelRecoveredEvent('channel.recovered')).toBe(true);
    expect(isChannelRecoveredEvent('channel.telegram_send_failed')).toBe(false);
    expect(isChannelRecoveredEvent('')).toBe(false);
  });
});

describe('excludeRecoveredChannelFailures', () => {
  it('drops the recovery row itself — it must never appear as an attention item', () => {
    const kept = excludeRecoveredChannelFailures([recoveredEvent()]);
    expect(kept).toEqual([]);
  });

  it('drops a failure whose channel recovered LATER, keeps one that recovered EARLIER', () => {
    const earlyFailure = failureEvent({ timestamp: '2026-08-11T08:00:00Z' });
    const recovery = recoveredEvent({ timestamp: '2026-08-11T08:30:00Z' });
    const laterFailure = failureEvent({ timestamp: '2026-08-11T09:00:00Z' });
    const kept = excludeRecoveredChannelFailures([earlyFailure, recovery, laterFailure]);
    expect(kept).toEqual([laterFailure]);
  });

  it('leaves unresolvable-channel rows untouched for the 24h proxy to judge', () => {
    const noChannel = failureEvent({ details: undefined });
    const kept = excludeRecoveredChannelFailures([noChannel, recoveredEvent()]);
    expect(kept).toEqual([noChannel]);
  });

  it('a different channel recovering does not affect this one', () => {
    const telegramFailure = failureEvent();
    const slackRecovered = recoveredEvent({ details: { channel_failure: { channel: 'slack' } } });
    const kept = excludeRecoveredChannelFailures([telegramFailure, slackRecovered]);
    expect(kept).toEqual([telegramFailure]);
  });

  it('empty input → empty output', () => {
    expect(excludeRecoveredChannelFailures([])).toEqual([]);
  });
});

describe('overviewCounts + isAllClear', () => {
  it('derives M/K straight from the same item list the list renders — they cannot drift apart', () => {
    const items = buildAttentionItems(
      [{ id: 'a1', title: 'x' }, { id: 'a2', title: 'y' }],
      [{ id: 't1', title: 'z' }],
      [{ id: 'f1', title: 'w' }],
    );
    const counts = overviewCounts(5, items);
    expect(counts).toEqual({ standby: 5, pendingDecisions: 3, undelivered: 1 });
    expect(isAllClear(counts)).toBe(false);
  });

  it('all-clear when there are no approvals/needs_human tasks/failures, regardless of standby count', () => {
    const counts = overviewCounts(3, []);
    expect(counts).toEqual({ standby: 3, pendingDecisions: 0, undelivered: 0 });
    expect(isAllClear(counts)).toBe(true);
  });

  it('is not "all clear" when only undelivered sends are outstanding', () => {
    const items = buildAttentionItems([], [], [{ id: 'f1', title: 'w' }]);
    expect(isAllClear(overviewCounts(0, items))).toBe(false);
  });
});
