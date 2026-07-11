import { describe, it, expect } from 'vitest';
import { parseCron, buildCron, describeCron, type CronParts } from './cron';

describe('parseCron', () => {
  it('parses hourly (minute set, hour *)', () => {
    expect(parseCron('0 * * * *')).toMatchObject({ mode: 'hourly', minute: 0 });
    expect(parseCron('30 * * * *')).toMatchObject({ mode: 'hourly', minute: 30 });
  });

  it('parses daily (minute + hour)', () => {
    expect(parseCron('0 9 * * *')).toMatchObject({ mode: 'daily', minute: 0, hour: 9 });
    expect(parseCron('30 8 * * *')).toMatchObject({ mode: 'daily', minute: 30, hour: 8 });
  });

  it('parses weekly (minute + hour + weekday)', () => {
    expect(parseCron('0 9 * * 1')).toMatchObject({ mode: 'weekly', minute: 0, hour: 9, weekday: 1 });
    expect(parseCron('15 14 * * 5')).toMatchObject({ mode: 'weekly', minute: 15, hour: 14, weekday: 5 });
  });

  it('parses interval (*/N)', () => {
    expect(parseCron('*/30 * * * *')).toMatchObject({ mode: 'interval', interval: 30 });
    expect(parseCron('*/5 * * * *')).toMatchObject({ mode: 'interval', interval: 5 });
  });

  it('falls back to custom for unsupported shapes', () => {
    expect(parseCron('0 0 1 * *').mode).toBe('custom'); // day-of-month
    expect(parseCron('not a cron').mode).toBe('custom');
    expect(parseCron('0 9 * * 8').mode).toBe('custom'); // weekday out of range
    expect(parseCron('99 * * * *').mode).toBe('custom'); // minute out of range
    expect(parseCron('').mode).toBe('custom');
  });
});

describe('buildCron', () => {
  const base: CronParts = { mode: 'hourly', minute: 0, hour: 9, weekday: 1, interval: 30, raw: '' };
  it('builds each mode', () => {
    expect(buildCron({ ...base, mode: 'hourly', minute: 15 })).toBe('15 * * * *');
    expect(buildCron({ ...base, mode: 'daily', minute: 30, hour: 8 })).toBe('30 8 * * *');
    expect(buildCron({ ...base, mode: 'weekly', minute: 0, hour: 9, weekday: 3 })).toBe('0 9 * * 3');
    expect(buildCron({ ...base, mode: 'interval', interval: 45 })).toBe('*/45 * * * *');
    expect(buildCron({ ...base, mode: 'custom', raw: '0 0 1 * *' })).toBe('0 0 1 * *');
  });
});

describe('round-trip parse ∘ build', () => {
  it('is stable for every friendly shape', () => {
    for (const cron of ['0 * * * *', '30 * * * *', '0 9 * * *', '30 8 * * *', '0 9 * * 1', '15 14 * * 5', '*/30 * * * *']) {
      expect(buildCron(parseCron(cron))).toBe(cron);
    }
  });
});

describe('describeCron', () => {
  it('describes each mode (English default)', () => {
    expect(describeCron('0 * * * *')).toBe('Every hour at :00');
    expect(describeCron('30 * * * *')).toBe('Every hour at :30');
    expect(describeCron('0 9 * * *')).toBe('Every day at 09:00');
    expect(describeCron('30 8 * * *')).toBe('Every day at 08:30');
    expect(describeCron('0 9 * * 1')).toBe('Every Monday at 09:00');
    expect(describeCron('15 14 * * 5')).toBe('Every Friday at 14:15');
    expect(describeCron('*/30 * * * *')).toBe('Every 30 minutes');
    expect(describeCron('0 0 1 * *')).toBe('Custom: 0 0 1 * *');
    expect(describeCron('garbage')).toBe('Custom: garbage');
  });

  it('honours custom labels', () => {
    const zh = describeCron('0 9 * * 1', {
      hourly: (mm) => `每小時第 ${mm} 分`,
      daily: (t) => `每天 ${t}`,
      weekly: (d, t) => `每${d} ${t}`,
      interval: (n) => `每 ${n} 分鐘`,
      custom: (raw) => `自訂：${raw}`,
      weekdays: ['週日', '週一', '週二', '週三', '週四', '週五', '週六'],
    });
    expect(zh).toBe('每週一 09:00');
  });
});
