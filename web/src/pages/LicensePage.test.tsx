import { describe, it, expect } from 'vitest';
import { classifyExpiry } from './LicensePage';

describe('classifyExpiry', () => {
  it('returns ok when expiry is months away', () => {
    expect(classifyExpiry(120).tone).toBe('ok');
    expect(classifyExpiry(31).tone).toBe('ok');
  });

  it('returns warning at the 30-day threshold', () => {
    expect(classifyExpiry(30).tone).toBe('warning');
    expect(classifyExpiry(15).tone).toBe('warning');
    expect(classifyExpiry(8).tone).toBe('warning');
  });

  it('returns critical at the 7-day threshold', () => {
    expect(classifyExpiry(7).tone).toBe('critical');
    expect(classifyExpiry(1).tone).toBe('critical');
    expect(classifyExpiry(0).tone).toBe('critical');
  });

  it('returns expired for negative days', () => {
    expect(classifyExpiry(-1).tone).toBe('expired');
    expect(classifyExpiry(-30).tone).toBe('expired');
  });

  it('returns unknown when value is null or undefined', () => {
    expect(classifyExpiry(null).tone).toBe('unknown');
    expect(classifyExpiry(undefined).tone).toBe('unknown');
  });

  it('emits a stable label id per bucket', () => {
    expect(classifyExpiry(60).labelId).toBe('license.expiry.ok');
    expect(classifyExpiry(20).labelId).toBe('license.expiry.warning');
    expect(classifyExpiry(3).labelId).toBe('license.expiry.critical');
    expect(classifyExpiry(-1).labelId).toBe('license.expiry.expired');
    expect(classifyExpiry(undefined).labelId).toBe('license.expiry.unknown');
  });
});
