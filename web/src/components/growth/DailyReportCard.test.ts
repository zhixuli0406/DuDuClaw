import { describe, it, expect } from 'vitest';
import '@/test/mocks';
import { shouldShowDailyReport, localDayKey } from './DailyReportCard';

describe('DailyReportCard gate (once per day)', () => {
  it('shows when never shown before', () => {
    expect(shouldShowDailyReport(null, '2026-07-10')).toBe(true);
  });

  it('does not show again the same day', () => {
    expect(shouldShowDailyReport('2026-07-10', '2026-07-10')).toBe(false);
  });

  it('shows again on a new day', () => {
    expect(shouldShowDailyReport('2026-07-09', '2026-07-10')).toBe(true);
  });

  it('localDayKey formats a stable YYYY-MM-DD in local time', () => {
    expect(localDayKey(new Date(2026, 6, 10, 23, 30))).toBe('2026-07-10');
    expect(localDayKey(new Date(2026, 0, 1, 0, 0))).toBe('2026-01-01');
  });
});
