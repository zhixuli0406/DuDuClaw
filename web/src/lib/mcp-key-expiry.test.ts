import { describe, it, expect } from 'vitest';
import { DEFAULT_EXPIRY, expiryPayload, tomorrow } from './mcp-key-expiry';

describe('expiryPayload (RM-134)', () => {
  const now = new Date(2026, 10, 13, 10, 0, 0).getTime();

  it('maps the presets to days', () => {
    expect(expiryPayload(DEFAULT_EXPIRY, now)).toEqual({ kind: 'days', days: 90 });
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: '30' }, now)).toEqual({ kind: 'days', days: 30 });
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: '365' }, now)).toEqual({ kind: 'days', days: 365 });
  });

  it('needs explicit confirmation for no expiry', () => {
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: 'never' }, now)).toBeNull();
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: 'never', confirmed: true }, now)).toEqual({
      kind: 'never',
      confirm: true,
    });
  });

  it('turns a future custom date into the end of that local day', () => {
    const payload = expiryPayload({ ...DEFAULT_EXPIRY, choice: 'custom', date: '2027-01-31' }, now);
    expect(payload).toEqual({ kind: 'date', at: new Date(2027, 0, 31, 23, 59, 59).toISOString() });
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: 'custom', date: '' }, now)).toBeNull();
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: 'custom', date: '2026-11-12' }, now)).toBeNull();
    expect(expiryPayload({ ...DEFAULT_EXPIRY, choice: 'custom', date: 'next week' }, now)).toBeNull();
  });

  it('offers tomorrow as the earliest custom date', () => {
    expect(tomorrow(new Date(2026, 11, 31, 12))).toBe('2027-01-01');
  });
});
