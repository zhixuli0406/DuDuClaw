import { describe, it, expect } from 'vitest';
import {
  formatCents,
  formatTokens,
  formatId,
  timeAgo,
  formatXp,
  formatCoins,
  formatDurationSaved,
} from './format';

describe('format helpers (dashboard-redesign §8)', () => {
  it('formatCents', () => {
    expect(formatCents(1234)).toBe('$12.34');
    expect(formatCents(0)).toBe('$0.00');
    expect(formatCents(null)).toBe('$0.00');
    expect(formatCents(undefined)).toBe('$0.00');
  });

  it('formatTokens with k/M suffix', () => {
    expect(formatTokens(999)).toBe('999');
    expect(formatTokens(1500)).toBe('1.5k');
    expect(formatTokens(2_500_000)).toBe('2.5M');
    expect(formatTokens(undefined)).toBe('0');
  });

  it('formatId is UTF-8 safe and shortens long ids', () => {
    expect(formatId('short')).toBe('short');
    expect(formatId('abcdefghijklmnop')).toBe('abcdef…mnop');
    expect(formatId(null)).toBe('—');
    // Multi-byte: must not split a character.
    const cjk = '一二三四五六七八九十十一十二';
    const out = formatId(cjk, 3, 2);
    expect(out).toContain('…');
    expect(out.startsWith('一二三')).toBe(true);
  });

  it('timeAgo returns compact unit tokens', () => {
    const now = 1_700_000_000_000;
    expect(timeAgo(new Date(now - 10_000), now)).toBe('now');
    expect(timeAgo(new Date(now - 5 * 60_000), now)).toBe('5m');
    expect(timeAgo(new Date(now - 2 * 3600_000), now)).toBe('2h');
    expect(timeAgo(new Date(now - 3 * 86_400_000), now)).toBe('3d');
    expect(timeAgo(null)).toBe('—');
    expect(timeAgo('not-a-date')).toBe('—');
  });

  it('formatXp groups thousands then M-suffixes', () => {
    expect(formatXp(0)).toBe('0');
    expect(formatXp(999)).toBe('999');
    expect(formatXp(1234)).toBe('1,234');
    expect(formatXp(2_500_000)).toBe('2.5M');
    expect(formatXp(-50)).toBe('0');
    expect(formatXp(null)).toBe('0');
  });

  it('formatCoins renders USD cents and rounds TWD', () => {
    expect(formatCoins(1234)).toBe('$12.34');
    expect(formatCoins(0)).toBe('$0.00');
    expect(formatCoins(null)).toBe('$0.00');
    expect(formatCoins(123_400, 'TWD')).toBe('NT$1,234');
    expect(formatCoins(1250, 'TWD')).toBe('NT$13'); // rounds 12.5 → 13
  });

  it('formatDurationSaved humanizes minutes into m/h/d tokens', () => {
    expect(formatDurationSaved(0)).toBe('0m');
    expect(formatDurationSaved(45)).toBe('45m');
    expect(formatDurationSaved(120)).toBe('2h');
    expect(formatDurationSaved(150)).toBe('2.5h');
    expect(formatDurationSaved(2880)).toBe('2d');
    expect(formatDurationSaved(-10)).toBe('0m');
    expect(formatDurationSaved(null)).toBe('0m');
  });
});
