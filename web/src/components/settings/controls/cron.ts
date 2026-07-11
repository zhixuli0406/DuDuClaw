// Pure cron helpers backing <ScheduleBuilder>. Kept UI-free so the parse /
// build / describe round-trip is unit-testable without React or intl.
//
// Supported "friendly" shapes (anything else → custom, shown as a raw text box):
//   hourly    →  M * * * *          every hour at minute M
//   daily     →  M H * * *          every day at HH:MM
//   weekly    →  M H * * D          every week on weekday D at HH:MM
//   interval  →  */N * * * *        every N minutes
//   custom    →  <raw 5-field cron> anything the four modes can't represent

export type CronMode = 'hourly' | 'daily' | 'weekly' | 'interval' | 'custom';

export interface CronParts {
  mode: CronMode;
  minute: number; // 0-59
  hour: number; // 0-23
  weekday: number; // 0-6 (0 = Sunday)
  interval: number; // minutes (interval mode)
  raw: string; // verbatim string (custom mode)
}

export const DEFAULT_CRON_PARTS: CronParts = {
  mode: 'hourly',
  minute: 0,
  hour: 9,
  weekday: 1,
  interval: 30,
  raw: '0 * * * *',
};

const isInt = (s: string) => /^\d+$/.test(s);
const inRange = (n: number, lo: number, hi: number) => n >= lo && n <= hi;

/** Parse a cron string into friendly parts. Unrecognised shapes → custom. */
export function parseCron(cron: string): CronParts {
  const raw = (cron ?? '').trim();
  const custom: CronParts = { ...DEFAULT_CRON_PARTS, mode: 'custom', raw: raw || '0 * * * *' };
  if (!raw) return custom;

  const fields = raw.split(/\s+/);
  if (fields.length !== 5) return custom;
  const [m, h, dom, mon, dow] = fields;

  // interval: every N minutes
  const iv = m.match(/^\*\/(\d+)$/);
  if (iv && h === '*' && dom === '*' && mon === '*' && dow === '*') {
    const interval = Number(iv[1]);
    if (inRange(interval, 1, 59)) {
      return { ...DEFAULT_CRON_PARTS, mode: 'interval', interval, raw };
    }
    return custom;
  }

  // The three time-of-day modes all require day-of-month / month = "*".
  if (dom !== '*' || mon !== '*') return custom;

  const minute = Number(m);
  if (isInt(m) && inRange(minute, 0, 59)) {
    // hourly: minute set, hour = *
    if (h === '*' && dow === '*') {
      return { ...DEFAULT_CRON_PARTS, mode: 'hourly', minute, raw };
    }
    const hour = Number(h);
    if (isInt(h) && inRange(hour, 0, 23)) {
      // daily: minute + hour, weekday = *
      if (dow === '*') {
        return { ...DEFAULT_CRON_PARTS, mode: 'daily', minute, hour, raw };
      }
      // weekly: minute + hour + numeric weekday
      const weekday = Number(dow);
      if (isInt(dow) && inRange(weekday, 0, 6)) {
        return { ...DEFAULT_CRON_PARTS, mode: 'weekly', minute, hour, weekday, raw };
      }
    }
  }
  return custom;
}

/** Build a cron string from friendly parts. */
export function buildCron(parts: CronParts): string {
  const { mode, minute, hour, weekday, interval, raw } = parts;
  switch (mode) {
    case 'hourly':
      return `${minute} * * * *`;
    case 'daily':
      return `${minute} ${hour} * * *`;
    case 'weekly':
      return `${minute} ${hour} * * ${weekday}`;
    case 'interval':
      return `*/${interval} * * * *`;
    case 'custom':
    default:
      return raw.trim();
  }
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** Labels used by {@link describeCron}. Defaults to English; UI passes intl. */
export interface CronLabels {
  hourly: (mm: string) => string;
  daily: (hhmm: string) => string;
  weekly: (day: string, hhmm: string) => string;
  interval: (n: number) => string;
  custom: (raw: string) => string;
  weekdays: readonly string[]; // length 7, index 0 = Sunday
}

const EN_LABELS: CronLabels = {
  hourly: (mm) => `Every hour at :${mm}`,
  daily: (hhmm) => `Every day at ${hhmm}`,
  weekly: (day, hhmm) => `Every ${day} at ${hhmm}`,
  interval: (n) => `Every ${n} minute${n === 1 ? '' : 's'}`,
  custom: (raw) => `Custom: ${raw}`,
  weekdays: ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'],
};

/**
 * Human-readable description of a cron string. Unparseable strings fall back to
 * the custom label (never throws). Pass `labels` for localized output.
 */
export function describeCron(cron: string, labels: CronLabels = EN_LABELS): string {
  const p = parseCron(cron);
  switch (p.mode) {
    case 'hourly':
      return labels.hourly(pad2(p.minute));
    case 'daily':
      return labels.daily(`${pad2(p.hour)}:${pad2(p.minute)}`);
    case 'weekly':
      return labels.weekly(labels.weekdays[p.weekday] ?? String(p.weekday), `${pad2(p.hour)}:${pad2(p.minute)}`);
    case 'interval':
      return labels.interval(p.interval);
    case 'custom':
    default:
      return labels.custom(p.raw);
  }
}
