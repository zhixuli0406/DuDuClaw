import { describe, it, expect, beforeEach } from 'vitest';
import {
  LAST_VISIT_KEY,
  SINCE_LAST_VISIT_MIN_GAP_MS,
  readLastVisit,
  writeLastVisit,
  shouldShowSinceLastVisit,
  summarizeSinceLastVisit,
  SINCE_VISIT_CATEGORIES,
} from './since-last-visit';

describe('readLastVisit / writeLastVisit', () => {
  beforeEach(() => localStorage.clear());

  it('round-trips a stamp through localStorage', () => {
    expect(readLastVisit()).toBeNull();
    writeLastVisit(12345);
    expect(readLastVisit()).toBe(12345);
  });

  it('treats an unparseable stored value as absent, never throws', () => {
    localStorage.setItem(LAST_VISIT_KEY, 'not-a-number');
    expect(readLastVisit()).toBeNull();
  });
});

describe('shouldShowSinceLastVisit', () => {
  const now = 1_000_000_000_000;

  it('false on a first visit (no prior stamp)', () => {
    expect(shouldShowSinceLastVisit(null, now)).toBe(false);
  });

  it('false when the gap is under the minimum (spec: <30min → 不顯示)', () => {
    expect(shouldShowSinceLastVisit(now - SINCE_LAST_VISIT_MIN_GAP_MS + 1, now)).toBe(false);
  });

  it('true right at and beyond the minimum gap', () => {
    expect(shouldShowSinceLastVisit(now - SINCE_LAST_VISIT_MIN_GAP_MS, now)).toBe(true);
    expect(shouldShowSinceLastVisit(now - SINCE_LAST_VISIT_MIN_GAP_MS - 1, now)).toBe(true);
  });

  it('false on clock skew (stamp is in the future relative to now)', () => {
    expect(shouldShowSinceLastVisit(now + 1000, now)).toBe(false);
  });
});

describe('summarizeSinceLastVisit', () => {
  const since = Date.parse('2026-08-10T00:00:00Z');
  const now = Date.parse('2026-08-11T00:00:00Z');
  const mid = (offsetMs: number) => new Date(since + offsetMs).toISOString();

  it('matches the spec example: aggregates into event-type counts, most-significant category first', () => {
    const events = [
      ...Array.from({ length: 12 }, () => ({ type: 'task_completed' as const, timestamp: mid(1000) })),
      ...Array.from({ length: 2 }, () => ({ type: 'skill_learned' as const, timestamp: mid(2000) })),
      { type: 'evolution_triggered' as const, timestamp: mid(3000) },
    ];
    expect(summarizeSinceLastVisit(events, since, now)).toEqual([
      { type: 'task_completed', count: 12 },
      { type: 'skill_learned', count: 2 },
      { type: 'evolution_triggered', count: 1 },
    ]);
  });

  it('drops zero-count categories entirely — no "0 件" noise', () => {
    const events = [{ type: 'task_completed' as const, timestamp: mid(1000) }];
    const summary = summarizeSinceLastVisit(events, since, now);
    expect(summary).toEqual([{ type: 'task_completed', count: 1 }]);
    expect(summary.some((s) => s.count === 0)).toBe(false);
  });

  it('excludes routine chatter not in SINCE_VISIT_CATEGORIES (task_created, agent_reply, autopilot_*)', () => {
    expect(SINCE_VISIT_CATEGORIES).not.toContain('task_created');
    expect(SINCE_VISIT_CATEGORIES).not.toContain('agent_reply');
    expect(SINCE_VISIT_CATEGORIES).not.toContain('autopilot_triggered');
    const events = [
      { type: 'task_created' as const, timestamp: mid(1000) },
      { type: 'agent_reply' as const, timestamp: mid(1000) },
      { type: 'autopilot_triggered' as const, timestamp: mid(1000) },
    ];
    expect(summarizeSinceLastVisit(events, since, now)).toEqual([]);
  });

  it('excludes events before the anchor or after "now", and unparseable timestamps', () => {
    const events = [
      { type: 'task_completed' as const, timestamp: new Date(since - 1000).toISOString() }, // before anchor
      { type: 'task_completed' as const, timestamp: new Date(now + 1000).toISOString() }, // after now
      { type: 'task_completed' as const, timestamp: 'garbage' },
    ];
    expect(summarizeSinceLastVisit(events, since, now)).toEqual([]);
  });

  it('no events at all → empty summary, not a placeholder row', () => {
    expect(summarizeSinceLastVisit([], since, now)).toEqual([]);
  });
});
