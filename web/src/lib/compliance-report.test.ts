import { describe, expect, it } from 'vitest';
import { complianceDateRange, complianceLanguage } from './compliance-report';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';

describe('compliance report requests', () => {
  it('uses Taipei calendar dates with an exclusive UTC end', () => {
    expect(complianceDateRange('2026-11-01', '2026-11-01')).toEqual({ from: '2026-10-31T16:00:00.000Z', until: '2026-11-01T16:00:00.000Z' });
  });
  it.each([['', '2026-11-01'], ['2026-11-01', ''], ['2026-02-30', '2026-03-01'], ['2026-11-02', '2026-11-01'], ['2025-01-01', '2026-01-02']])('refuses invalid or excessive dates: %s through %s', (from, until) => {
    expect(() => complianceDateRange(from, until)).toThrow();
  });
  it('uses a supported report language', () => {
    expect(complianceLanguage('zh-TW')).toBe('zh-TW');
    expect(complianceLanguage('ja-JP')).toBe('ja-JP');
    expect(complianceLanguage('en')).toBe('en');
    expect(complianceLanguage('unknown')).toBe('zh-TW');
  });
  it('has matching translated user-facing messages', () => {
    const keys = Object.keys(zh).filter((key) => key.startsWith('compliance.'));
    expect(keys.length).toBeGreaterThan(0);
    for (const key of keys) for (const messages of [en, zh, ja]) expect(Object.hasOwn(messages, key)).toBe(true);
  });
});
