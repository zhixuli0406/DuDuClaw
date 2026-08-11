import { describe, it, expect } from 'vitest';
import { isDevPanelCriticalEvent, latestCriticalTimestamp } from './devpanel-alerts';
import type { UnifiedAuditEvent } from './api';

function event(overrides: Partial<UnifiedAuditEvent>): UnifiedAuditEvent {
  return {
    timestamp: '2026-08-11T00:00:00Z',
    source: 'channel_failure',
    event_type: 'channel.Unknown',
    agent_id: 'nova',
    severity: 'warning',
    summary: '',
    details: {},
    ...overrides,
  };
}

describe('isDevPanelCriticalEvent', () => {
  it('always counts a backend-labeled critical event, regardless of source', () => {
    expect(isDevPanelCriticalEvent(event({ source: 'security', severity: 'critical' }))).toBe(true);
  });

  it('does not count a plain warning/info event', () => {
    expect(isDevPanelCriticalEvent(event({ severity: 'warning' }))).toBe(false);
    expect(isDevPanelCriticalEvent(event({ severity: 'info' }))).toBe(false);
  });

  it('escalates a channel_failure row with reason "Billing" to critical', () => {
    expect(
      isDevPanelCriticalEvent(
        event({ source: 'channel_failure', severity: 'warning', details: { channel_failure: { reason: 'Billing' } } }),
      ),
    ).toBe(true);
  });

  it('escalates a channel_failure row with reason "AccountsCoolingDownLong" (24h billing-class cooldown)', () => {
    expect(
      isDevPanelCriticalEvent(
        event({
          source: 'channel_failure',
          severity: 'warning',
          details: { channel_failure: { reason: 'AccountsCoolingDownLong' } },
        }),
      ),
    ).toBe(true);
  });

  it('does NOT escalate an unrelated channel_failure reason', () => {
    expect(
      isDevPanelCriticalEvent(
        event({ source: 'channel_failure', severity: 'warning', details: { channel_failure: { reason: 'Timeout' } } }),
      ),
    ).toBe(false);
  });

  it('does not false-positive on a recovery row (reason: "recovered")', () => {
    expect(
      isDevPanelCriticalEvent(
        event({
          source: 'channel_failure',
          severity: 'info',
          details: { channel_failure: { reason: 'recovered', event: 'channel_recovered' } },
        }),
      ),
    ).toBe(false);
  });

  it('is an exact match, not a substring match — "BillingSomethingElse" does not qualify', () => {
    expect(
      isDevPanelCriticalEvent(
        event({
          source: 'channel_failure',
          severity: 'warning',
          details: { channel_failure: { reason: 'BillingSomethingElse' } },
        }),
      ),
    ).toBe(false);
  });

  it('does not escalate a budget-class reason on a non-channel_failure source', () => {
    expect(
      isDevPanelCriticalEvent(
        event({ source: 'tool_call', severity: 'warning', details: { channel_failure: { reason: 'Billing' } } }),
      ),
    ).toBe(false);
  });

  it('tolerates a missing/malformed details.channel_failure without throwing', () => {
    expect(isDevPanelCriticalEvent(event({ source: 'channel_failure', details: {} }))).toBe(false);
    expect(
      isDevPanelCriticalEvent(event({ source: 'channel_failure', details: { channel_failure: null } })),
    ).toBe(false);
  });
});

describe('latestCriticalTimestamp', () => {
  it('returns null when nothing qualifies', () => {
    expect(latestCriticalTimestamp([event({}), event({ severity: 'warning' })])).toBeNull();
  });

  it('returns the newest timestamp among qualifying events, independent of input order', () => {
    const events = [
      event({ source: 'security', severity: 'critical', timestamp: '2026-08-10T00:00:00Z' }),
      event({ source: 'security', severity: 'critical', timestamp: '2026-08-11T12:00:00Z' }),
      event({ source: 'security', severity: 'critical', timestamp: '2026-08-09T00:00:00Z' }),
    ];
    expect(latestCriticalTimestamp(events)).toBe('2026-08-11T12:00:00Z');
  });

  it('ignores non-qualifying events mixed in with qualifying ones', () => {
    const events = [
      event({ severity: 'warning', timestamp: '2026-08-12T00:00:00Z' }),
      event({ source: 'security', severity: 'critical', timestamp: '2026-08-10T00:00:00Z' }),
    ];
    expect(latestCriticalTimestamp(events)).toBe('2026-08-10T00:00:00Z');
  });

  it('returns null for an empty list', () => {
    expect(latestCriticalTimestamp([])).toBeNull();
  });
});
