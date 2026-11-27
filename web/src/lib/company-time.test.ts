import { beforeEach, describe, expect, it, vi } from 'vitest';

const companyTimezoneMock = vi.hoisted(() => vi.fn());
vi.mock('./api', () => ({ api: { system: { companyTimezone: companyTimezoneMock } } }));

import {
  effectiveZone, formatDateTime, getCompanyTimeZone, localDateToInstant,
  nextLocalMidnight, resetCompanyTimeZone, supportedTimeZones, useCompanyTimeStore, zoneLabel,
} from './company-time';

beforeEach(() => {
  vi.clearAllMocks();
  resetCompanyTimeZone();
});

describe('company time formatting', () => {
  it('formats an instant in the explicit company zone', () => {
    expect(formatDateTime('2026-11-27T01:30:00Z', {
      zone: 'Asia/Taipei', locale: 'en-US', timeStyle: 'short',
    })).toContain('9:30');
  });

  it('uses the store zone unless an explicit zone overrides it', () => {
    useCompanyTimeStore.setState({ status: 'set', zone: 'Asia/Taipei' });
    expect(getCompanyTimeZone()).toBe('Asia/Taipei');
    expect(formatDateTime('2026-11-27T01:30:00Z', { locale: 'en-US', timeStyle: 'short' })).toContain('9:30');
    expect(formatDateTime('2026-11-27T01:30:00Z', {
      zone: 'UTC', locale: 'en-US', timeStyle: 'short',
    })).toContain('1:30');
    useCompanyTimeStore.setState({ status: 'invalid' });
    expect(getCompanyTimeZone()).toBeUndefined();
  });

  it('uses browser defaults for an absent company zone', () => {
    expect(effectiveZone('Asia/Taipei')).toBe('Asia/Taipei');
    expect(effectiveZone(null)).toBeUndefined();
    expect(effectiveZone('')).toBeUndefined();
    expect(zoneLabel()).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
    expect(zoneLabel('Asia/Taipei')).toBe('Asia/Taipei');
  });

  it('renders missing and invalid instants as a dash', () => {
    for (const value of [null, undefined, '', 'invalid', NaN, new Date(NaN)]) {
      expect(formatDateTime(value)).toBe('—');
    }
  });

  it('offers UTC and the company region in the zone list', () => {
    expect(supportedTimeZones()).toContain('UTC');
    expect(supportedTimeZones()).toContain('Asia/Taipei');
  });
});

describe('local calendar boundaries', () => {
  it('converts Taipei midnight into an instant', () => {
    expect(localDateToInstant('2026-11-27', 'Asia/Taipei')).toBe(Date.parse('2026-11-26T16:00:00Z'));
  });

  it('uses a 23 hour day at the spring DST transition', () => {
    expect(nextLocalMidnight('2026-03-08', 'America/New_York')
      - localDateToInstant('2026-03-08', 'America/New_York')).toBe(23 * 60 * 60 * 1000);
  });

  it('uses a 25 hour day at the autumn DST transition', () => {
    expect(nextLocalMidnight('2026-11-01', 'America/New_York')
      - localDateToInstant('2026-11-01', 'America/New_York')).toBe(25 * 60 * 60 * 1000);
  });

  it('advances the calendar across the end of a year', () => {
    expect(nextLocalMidnight('2026-12-31', 'Asia/Taipei')).toBe(Date.parse('2026-12-31T16:00:00Z'));
  });

  it.each(['invalid', '2026-2-01', '2026-02-29', '2026-04-31', '2026-13-01', '2026-00-01'])('rejects invalid date %s', (date) => {
    expect(() => localDateToInstant(date, 'Asia/Taipei')).toThrow('INVALID_DATE');
    expect(() => nextLocalMidnight(date, 'Asia/Taipei')).toThrow('INVALID_DATE');
  });
});

describe('company time store', () => {
  it('loads the effective zone and upgrade marker', async () => {
    companyTimezoneMock.mockResolvedValue({ timezone: 'Asia/Taipei', status: 'set', source: 'detected_at_upgrade' });
    await useCompanyTimeStore.getState().load();
    expect(useCompanyTimeStore.getState()).toMatchObject({ zone: 'Asia/Taipei', status: 'set', source: 'detected_at_upgrade' });
  });

  it('keeps unknown status without throwing on load failure', async () => {
    useCompanyTimeStore.setState({ zone: 'Asia/Taipei', status: 'set', source: 'detected_at_upgrade' });
    companyTimezoneMock.mockRejectedValue(new Error('offline'));
    await expect(useCompanyTimeStore.getState().load()).resolves.toBeUndefined();
    expect(useCompanyTimeStore.getState()).toMatchObject({ zone: null, status: 'unknown', source: null });
  });

  it('does not restore a departed session from an in-flight read', async () => {
    let resolve!: (result: { timezone: string; status: 'set'; source: null }) => void;
    companyTimezoneMock.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const pending = useCompanyTimeStore.getState().load();
    await vi.waitFor(() => expect(resolve).toBeTypeOf('function'));
    resetCompanyTimeZone();
    resolve({ timezone: 'Asia/Taipei', status: 'set', source: null });
    await pending;
    expect(getCompanyTimeZone()).toBeUndefined();
    expect(useCompanyTimeStore.getState().status).toBe('unknown');
  });
});
