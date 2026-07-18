import { useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';

const controlClass =
  'h-8 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 text-sm text-foreground placeholder:text-muted-foreground outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30';

export type DurationUnit = 'sec' | 'min' | 'hour';

const UNIT_SECONDS: Record<DurationUnit, number> = { sec: 1, min: 60, hour: 3600 };

/** Pick the largest unit that represents `seconds` without a fractional part. */
export function bestUnit(seconds: number, units: readonly DurationUnit[]): DurationUnit {
  const ordered = [...units].sort((a, b) => UNIT_SECONDS[b] - UNIT_SECONDS[a]);
  for (const u of ordered) {
    if (seconds % UNIT_SECONDS[u] === 0) return u;
  }
  return units[0];
}

/**
 * DurationField — a number + unit (seconds / minutes / hours) input that always
 * stores a canonical number of seconds via `onChange`. Lets a user enter "5
 * minutes" instead of "300 seconds" while the config stays in seconds.
 */
export function DurationField({
  seconds,
  onChange,
  units = ['sec', 'min', 'hour'],
  min = 0,
  disabled,
  className,
  id,
}: {
  seconds: number;
  onChange: (seconds: number) => void;
  units?: readonly DurationUnit[];
  min?: number;
  disabled?: boolean;
  className?: string;
  id?: string;
}) {
  const intl = useIntl();
  const [unit, setUnit] = useState<DurationUnit>(() => bestUnit(seconds, units));
  const amount = useMemo(() => seconds / UNIT_SECONDS[unit], [seconds, unit]);

  const unitLabel: Record<DurationUnit, string> = {
    sec: intl.formatMessage({ id: 'controls.duration.sec' }),
    min: intl.formatMessage({ id: 'controls.duration.min' }),
    hour: intl.formatMessage({ id: 'controls.duration.hour' }),
  };

  return (
    <div className={cn('flex items-center gap-2', className)}>
      <input
        id={id}
        type="number"
        min={min}
        step="any"
        disabled={disabled}
        value={Number.isFinite(amount) ? amount : ''}
        onChange={(e) => {
          const v = Number(e.target.value);
          onChange(Number.isFinite(v) ? Math.round(v * UNIT_SECONDS[unit]) : 0);
        }}
        className={cn(controlClass, 'w-28')}
      />
      <select
        value={unit}
        disabled={disabled}
        onChange={(e) => setUnit(e.target.value as DurationUnit)}
        className={cn(controlClass, 'w-auto min-w-[6rem]')}
      >
        {units.map((u) => (
          <option key={u} value={u}>
            {unitLabel[u]}
          </option>
        ))}
      </select>
    </div>
  );
}
