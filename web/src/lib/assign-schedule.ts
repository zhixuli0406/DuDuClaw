import type { IntlShape } from 'react-intl';
import type { AssignScheduleInput, AssignScheduleSpec } from '@/lib/api';
import { formatError } from '@/lib/toast';

/**
 * Scheduled assignments (assign later / on a repeat): the form value of the
 * "when" picker, its wire shape, and the words used to show a schedule.
 * The gateway re-checks the creator's right before every run.
 */

export type WhenKind = 'now' | 'once' | 'daily' | 'weekdays' | 'weekly' | 'hourly' | 'cron';

export interface WhenValue {
  kind: WhenKind;
  /** `once`: a `datetime-local` value (`YYYY-MM-DDTHH:MM`). */
  at: string;
  /** `daily` / `weekdays` / `weekly`: `HH:MM`. */
  time: string;
  /** `weekly`: 1 = Monday … 7 = Sunday. */
  days: number[];
  /** `hourly`: minute past the hour. */
  minute: number;
  /** `cron` (Admins): the expression. */
  expr: string;
}

export const WHEN_NOW: WhenValue = { kind: 'now', at: '', time: '09:00', days: [1], minute: 0, expr: '' };

/** The viewer's IANA zone (schedule times are read in it). */
export function viewerTimezone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined;
  } catch {
    return undefined;
  }
}

/** The wire shape, or `null` for "now". Throws `Error('incomplete')` when a
 *  required field is empty. */
export function toScheduleInput(v: WhenValue): AssignScheduleInput | null {
  switch (v.kind) {
    case 'now':
      return null;
    case 'once':
      if (!v.at) throw new Error('incomplete');
      return { kind: 'once', at: v.at };
    case 'daily':
    case 'weekdays':
      if (!v.time) throw new Error('incomplete');
      return { kind: v.kind, time: v.time };
    case 'weekly':
      if (!v.time || v.days.length === 0) throw new Error('incomplete');
      return { kind: 'weekly', days: [...v.days].sort((a, b) => a - b), time: v.time };
    case 'hourly':
      return { kind: 'hourly', minute: Math.min(59, Math.max(0, Math.trunc(v.minute) || 0)) };
    case 'cron':
      if (!v.expr.trim()) throw new Error('incomplete');
      return { kind: 'cron', expr: v.expr.trim() };
  }
}

const pad = (n: number) => String(n).padStart(2, '0');

/** "Every Monday, Wednesday at 09:00" in the viewer's language. */
export function describeSchedule(intl: IntlShape, s: AssignScheduleSpec, timeZone?: string | null): string {
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: `assignSchedule.describe.${id}` }, values);
  switch (s.kind) {
    case 'once':
      return t('once', { at: formatWhen(intl, s.at, timeZone) });
    case 'hourly':
      return t('hourly', { minute: pad(s.minute) });
    case 'daily':
      return t('daily', { time: `${pad(s.hour)}:${pad(s.minute)}` });
    case 'weekdays':
      return t('weekdays', { time: `${pad(s.hour)}:${pad(s.minute)}` });
    case 'weekly': {
      const days = s.days.map((d) => intl.formatMessage({ id: `assignSchedule.day.${d}` })).join(
        intl.formatMessage({ id: 'assignSchedule.daySeparator' }),
      );
      return t('weekly', { days, time: `${pad(s.hour)}:${pad(s.minute)}` });
    }
    case 'cron':
      return t('cron', { expr: s.expr });
  }
}

/** An instant as a short local date-time, in the schedule's zone when known. */
export function formatWhen(intl: IntlShape, iso: string | null | undefined, timeZone?: string | null): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  try {
    return intl.formatDate(d, {
      month: 'numeric',
      day: 'numeric',
      weekday: 'short',
      hour: '2-digit',
      minute: '2-digit',
      hour12: false,
      timeZone: timeZone || undefined,
    });
  } catch {
    return intl.formatDate(d, { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  }
}

const KNOWN_ERRORS = [
  'too_many_schedules',
  'too_frequent',
  'cron_admin_only',
  'time_passed',
  'too_far_ahead',
  'invalid_schedule',
  'invalid_timezone',
  'never_fires',
  'risk_boundary_admin_only',
  'account_required',
  'creator_lost_access',
  'not_found',
  'not_applicable',
  'schedule_restricted',
  'store_error',
] as const;

/** A gateway refusal (`<code>[: detail]`) in the viewer's words. */
export function scheduleErrorText(intl: IntlShape, e: unknown): string {
  const raw = (e instanceof Error ? e.message : typeof e === 'string' ? e : '').trim();
  const code = KNOWN_ERRORS.find((c) => raw === c || raw.startsWith(`${c}:`));
  if (!code) return formatError(e);
  const n = /(\d+)/.exec(raw.slice(code.length))?.[1] ?? '';
  return intl.formatMessage({ id: `assignSchedule.error.${code}` }, { n });
}
