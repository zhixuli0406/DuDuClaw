import { describe, expect, it } from 'vitest';
import { createIntl } from 'react-intl';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import { formatError } from '@/lib/toast';
import { WHEN_NOW, describeSchedule, scheduleErrorText, toScheduleInput } from './assign-schedule';

const intlEn = createIntl({ locale: 'en', messages: en as Record<string, string> });
const intlZh = createIntl({ locale: 'zh-TW', messages: zh as Record<string, string> });

describe('assign-schedule', () => {
  it('turns the form value into the wire shape', () => {
    expect(toScheduleInput(WHEN_NOW)).toBeNull();
    expect(toScheduleInput({ ...WHEN_NOW, kind: 'weekly', days: [5, 1], time: '08:30' })).toEqual({
      kind: 'weekly',
      days: [1, 5],
      time: '08:30',
    });
    expect(toScheduleInput({ ...WHEN_NOW, kind: 'hourly', minute: 75 })).toEqual({ kind: 'hourly', minute: 59 });
    expect(toScheduleInput({ ...WHEN_NOW, kind: 'once', at: '2026-10-25T14:00' })).toEqual({
      kind: 'once',
      at: '2026-10-25T14:00',
    });
    expect(() => toScheduleInput({ ...WHEN_NOW, kind: 'once', at: '' })).toThrow('incomplete');
    expect(() => toScheduleInput({ ...WHEN_NOW, kind: 'weekly', days: [] })).toThrow('incomplete');
  });

  it('describes schedules in the viewer language', () => {
    expect(describeSchedule(intlZh, { kind: 'weekly', days: [1, 3], hour: 9, minute: 0 })).toBe('每週一、三 09:00');
    expect(describeSchedule(intlEn, { kind: 'weekdays', hour: 18, minute: 5 })).toBe('Weekdays at 18:05');
    expect(describeSchedule(intlZh, { kind: 'hourly', minute: 0 })).toBe('每小時 00 分');
  });

  it('maps gateway refusal codes to words', () => {
    expect(scheduleErrorText(intlZh, new Error('store_error: invalid scheduling configuration'))).toBe(
      '排程暫時無法處理，請稍後再試，或請管理員確認排程設定。',
    );
    expect(scheduleErrorText(intlZh, new Error('too_many_schedules: at most 20 active or paused schedules'))).toBe(
      '排程已達上限（20 個，含暫停中的），請先刪掉用不到的。',
    );
    expect(scheduleErrorText(intlEn, new Error('too_frequent: the shortest interval is 60 minutes'))).toBe(
      'That runs too often; the shortest interval is 60 minutes.',
    );
    // Anything else falls back to the app's generic error wording.
    const other = new Error('something else');
    expect(scheduleErrorText(intlEn, other)).toBe(formatError(other));
  });
});
