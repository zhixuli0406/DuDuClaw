import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { controlClass } from '@/components/ui';
import {
  parseCron,
  buildCron,
  describeCron,
  DEFAULT_CRON_PARTS,
  type CronMode,
  type CronParts,
  type CronLabels,
} from './cron';

const MODES: CronMode[] = ['hourly', 'daily', 'interval', 'weekly', 'custom'];

function pad2(n: number) {
  return String(n).padStart(2, '0');
}

/**
 * ScheduleBuilder — a plain-language cron builder. Pick "every hour / every day
 * at HH:MM / every N minutes / every week on … / custom", and it emits the
 * matching cron string. Any cron it can't express in a friendly mode (or that
 * the user types by hand) falls back to a raw text box under "custom", so no
 * schedule is ever unrepresentable. A live description reads the schedule back.
 */
export function ScheduleBuilder({
  value,
  onChange,
  className,
}: {
  value: string;
  onChange: (cron: string) => void;
  className?: string;
}) {
  const intl = useIntl();
  const [parts, setParts] = useState<CronParts>(() => parseCron(value));
  const lastValue = useRef(value);

  // Re-parse when the value changes from outside (agent switch / reload).
  useEffect(() => {
    if (value !== lastValue.current) {
      lastValue.current = value;
      setParts(parseCron(value));
    }
  }, [value]);

  const emit = (next: CronParts) => {
    setParts(next);
    const cron = buildCron(next);
    lastValue.current = cron;
    onChange(cron);
  };

  const labels: CronLabels = {
    hourly: (mm) => intl.formatMessage({ id: 'controls.cron.desc.hourly' }, { mm }),
    daily: (t) => intl.formatMessage({ id: 'controls.cron.desc.daily' }, { time: t }),
    weekly: (d, t) => intl.formatMessage({ id: 'controls.cron.desc.weekly' }, { day: d, time: t }),
    interval: (n) => intl.formatMessage({ id: 'controls.cron.desc.interval' }, { n }),
    custom: (raw) => intl.formatMessage({ id: 'controls.cron.desc.custom' }, { raw }),
    weekdays: [0, 1, 2, 3, 4, 5, 6].map((i) => intl.formatMessage({ id: `controls.cron.weekday.${i}` })),
  };

  const onModeChange = (mode: CronMode) => {
    // Seed sensible values when switching in; keep prior time fields.
    if (mode === 'custom') {
      emit({ ...parts, mode: 'custom', raw: buildCron({ ...parts, mode: parts.mode === 'custom' ? 'hourly' : parts.mode }) || DEFAULT_CRON_PARTS.raw });
    } else {
      emit({ ...parts, mode });
    }
  };

  const timeValue = `${pad2(parts.hour)}:${pad2(parts.minute)}`;
  const onTimeChange = (t: string) => {
    const [h, m] = t.split(':').map((x) => Number(x));
    emit({ ...parts, hour: Number.isFinite(h) ? h : 0, minute: Number.isFinite(m) ? m : 0 });
  };

  const modeLabel: Record<CronMode, string> = {
    hourly: intl.formatMessage({ id: 'controls.cron.mode.hourly' }),
    daily: intl.formatMessage({ id: 'controls.cron.mode.daily' }),
    interval: intl.formatMessage({ id: 'controls.cron.mode.interval' }),
    weekly: intl.formatMessage({ id: 'controls.cron.mode.weekly' }),
    custom: intl.formatMessage({ id: 'controls.cron.mode.custom' }),
  };

  return (
    <div className={cn('space-y-2', className)}>
      <div className="flex flex-wrap items-center gap-2">
        <select
          value={parts.mode}
          onChange={(e) => onModeChange(e.target.value as CronMode)}
          className={cn(controlClass, 'w-auto min-w-[9rem]')}
        >
          {MODES.map((m) => (
            <option key={m} value={m}>
              {modeLabel[m]}
            </option>
          ))}
        </select>

        {parts.mode === 'hourly' && (
          <label className="flex items-center gap-1.5 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'controls.cron.atMinute' })}
            <input
              type="number"
              min={0}
              max={59}
              value={parts.minute}
              onChange={(e) => emit({ ...parts, minute: Math.max(0, Math.min(59, Number(e.target.value) || 0)) })}
              className={cn(controlClass, 'w-20 text-center')}
            />
          </label>
        )}

        {parts.mode === 'interval' && (
          <label className="flex items-center gap-1.5 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'controls.cron.everyN' })}
            <input
              type="number"
              min={1}
              max={59}
              value={parts.interval}
              onChange={(e) => emit({ ...parts, interval: Math.max(1, Math.min(59, Number(e.target.value) || 1)) })}
              className={cn(controlClass, 'w-20 text-center')}
            />
            {intl.formatMessage({ id: 'controls.duration.min' })}
          </label>
        )}

        {(parts.mode === 'daily' || parts.mode === 'weekly') && (
          <input
            type="time"
            value={timeValue}
            onChange={(e) => onTimeChange(e.target.value)}
            className={cn(controlClass, 'w-auto')}
          />
        )}

        {parts.mode === 'weekly' && (
          <select
            value={parts.weekday}
            onChange={(e) => emit({ ...parts, weekday: Number(e.target.value) })}
            className={cn(controlClass, 'w-auto min-w-[6rem]')}
          >
            {[1, 2, 3, 4, 5, 6, 0].map((d) => (
              <option key={d} value={d}>
                {intl.formatMessage({ id: `controls.cron.weekday.${d}` })}
              </option>
            ))}
          </select>
        )}

        {parts.mode === 'custom' && (
          <input
            type="text"
            value={parts.raw}
            onChange={(e) => emit({ ...parts, raw: e.target.value })}
            placeholder="0 * * * *"
            className={cn(controlClass, 'w-48 font-mono')}
          />
        )}
      </div>

      <p className="text-xs text-stone-400 dark:text-stone-500">
        {describeCron(buildCron(parts), labels)}
        <span className="ml-2 font-mono text-stone-300 dark:text-stone-600">{buildCron(parts)}</span>
      </p>
    </div>
  );
}
