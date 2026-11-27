import { create } from 'zustand';

interface CompanyTimeState {
  zone: string | null;
  status: 'set' | 'unset' | 'invalid' | 'unknown';
  source: string | null;
  load: () => Promise<void>;
}

let requestEpoch = 0;
export const useCompanyTimeStore = create<CompanyTimeState>((set) => ({
  zone: null,
  status: 'unknown',
  source: null,
  load: async () => {
    const epoch = ++requestEpoch;
    try {
      const { api } = await import('./api');
      const result = await api.system.companyTimezone();
      if (epoch === requestEpoch) {
        set({ zone: result.timezone, status: result.status, source: result.source });
      }
    } catch {
      if (epoch === requestEpoch) set({ zone: null, status: 'unknown', source: null });
    }
  },
}));

/** Forget the previous session, including any read still in flight. */
export function resetCompanyTimeZone(): void {
  requestEpoch += 1;
  useCompanyTimeStore.setState({ zone: null, status: 'unknown', source: null });
}

export function useCompanyTimeZone() {
  return useCompanyTimeStore();
}

export function getCompanyTimeZone(): string | undefined {
  const { zone, status } = useCompanyTimeStore.getState();
  return status === 'set' ? effectiveZone(zone) : undefined;
}

export function effectiveZone(companyZone?: string | null): string | undefined {
  return companyZone || undefined;
}

export function formatDateTime(
  value: string | number | Date | null | undefined,
  opts: Intl.DateTimeFormatOptions & { zone?: string; locale?: string } = {},
): string {
  if (value == null) return '—';
  const date = value instanceof Date ? value : new Date(value);
  if (!Number.isFinite(date.getTime())) return '—';
  const { zone, locale, ...format } = opts;
  const defaults: Intl.DateTimeFormatOptions = Object.keys(format).length === 0
    ? { dateStyle: 'medium', timeStyle: 'short' } : {};
  return new Intl.DateTimeFormat(locale, {
    ...defaults, ...format, timeZone: zone ?? getCompanyTimeZone(),
  }).format(date);
}

export function zoneLabel(zone?: string): string {
  return zone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
}

function calendarDate(date: string): Date {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error('INVALID_DATE');
  const parsed = new Date(`${date}T00:00:00Z`);
  if (!Number.isFinite(parsed.getTime()) || parsed.toISOString().slice(0, 10) !== date) {
    throw new Error('INVALID_DATE');
  }
  return parsed;
}

/** Resolve local midnight using the offset at the candidate instant, not today's offset. */
export function localDateToInstant(date: string, zone?: string): number {
  const target = calendarDate(date).getTime();
  const formatter = new Intl.DateTimeFormat('en-US-u-ca-iso8601-nu-latn', {
    timeZone: zone,
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23',
  });
  let instant = target;
  for (let i = 0; i < 8; i++) {
    const parts = formatter.formatToParts(instant);
    const part = (type: Intl.DateTimeFormatPartTypes) => Number(parts.find((p) => p.type === type)?.value);
    const local = new Date(0);
    local.setUTCFullYear(part('year'), part('month') - 1, part('day'));
    local.setUTCHours(part('hour'), part('minute'), part('second'), 0);
    const correction = target - local.getTime();
    if (correction === 0) return instant;
    instant += correction;
  }
  // Some zones skip midnight (or an entire date). A nonexistent midnight
  // cannot round-trip to the requested calendar date.
  throw new Error('INVALID_DATE');
}

export function nextLocalMidnight(date: string, zone?: string): number {
  const next = calendarDate(date);
  next.setUTCDate(next.getUTCDate() + 1);
  return localDateToInstant(next.toISOString().slice(0, 10), zone);
}

export function supportedTimeZones(): string[] {
  if (typeof Intl.supportedValuesOf === 'function') {
    // UTC is valid but omitted by the enumeration on some browsers.
    return [...new Set(['UTC', ...Intl.supportedValuesOf('timeZone')])];
  }
  return [
    'UTC', 'Asia/Taipei', 'Asia/Tokyo', 'Asia/Singapore', 'Asia/Hong_Kong',
    'America/New_York', 'America/Los_Angeles', 'Europe/London',
  ];
}
