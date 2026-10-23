import { useIntl } from 'react-intl';
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/mds';
import type { WhenKind, WhenValue } from '@/lib/assign-schedule';

/**
 * ScheduleField — "when should this run" in the assign panel: now (the
 * default), once at a time, or on a repeat. A custom cron expression is
 * offered to Admins only (the gateway refuses it from anyone else).
 */
export function ScheduleField({
  value,
  onChange,
  isAdmin,
  disabled,
}: {
  value: WhenValue;
  onChange: (next: WhenValue) => void;
  isAdmin: boolean;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `assign.when.${id}` });
  const kinds: WhenKind[] = ['now', 'once', 'daily', 'weekdays', 'weekly', 'hourly', ...(isAdmin ? (['cron'] as const) : [])];
  const set = (patch: Partial<WhenValue>) => onChange({ ...value, ...patch });
  const toggleDay = (d: number) =>
    set({ days: value.days.includes(d) ? value.days.filter((x) => x !== d) : [...value.days, d] });

  return (
    <div className="space-y-2">
      <span className="text-xs font-medium text-muted-foreground">{t('label')}</span>
      <div className="flex flex-wrap items-center gap-2">
        <Select value={value.kind} onValueChange={(v) => set({ kind: v as WhenKind })}>
          <SelectTrigger className="w-full sm:w-56" aria-label={t('label')} disabled={disabled}>
            <SelectValue>{t(`kind.${value.kind}`)}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            {kinds.map((k) => (
              <SelectItem key={k} value={k}>
                {t(`kind.${k}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        {value.kind === 'once' && (
          <Input
            type="datetime-local"
            aria-label={t('at')}
            className="w-full sm:w-60"
            value={value.at}
            disabled={disabled}
            onChange={(e) => set({ at: e.target.value })}
          />
        )}
        {(value.kind === 'daily' || value.kind === 'weekdays' || value.kind === 'weekly') && (
          <Input
            type="time"
            aria-label={t('time')}
            className="w-full sm:w-36"
            value={value.time}
            disabled={disabled}
            onChange={(e) => set({ time: e.target.value })}
          />
        )}
        {value.kind === 'hourly' && (
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            {t('minute')}
            <Input
              type="number"
              min={0}
              max={59}
              className="w-20"
              value={value.minute}
              disabled={disabled}
              onChange={(e) => set({ minute: Number(e.target.value) })}
            />
          </label>
        )}
        {value.kind === 'cron' && (
          <Input
            aria-label={t('kind.cron')}
            placeholder="0 9 * * 1"
            className="w-full font-mono sm:w-56"
            value={value.expr}
            disabled={disabled}
            spellCheck={false}
            onChange={(e) => set({ expr: e.target.value })}
          />
        )}
      </div>

      {value.kind === 'weekly' && (
        <div role="group" aria-label={t('days')} className="flex flex-wrap gap-1.5">
          {[1, 2, 3, 4, 5, 6, 7].map((d) => {
            const on = value.days.includes(d);
            return (
              <button
                key={d}
                type="button"
                aria-pressed={on}
                disabled={disabled}
                onClick={() => toggleDay(d)}
                className={`h-8 min-w-9 rounded-md border px-2 text-sm outline-none transition-colors focus-visible:ring-3 focus-visible:ring-ring/50 ${
                  on ? 'border-primary bg-primary text-primary-foreground' : 'border-surface-border text-muted-foreground hover:text-foreground'
                }`}
              >
                {intl.formatMessage({ id: `assignSchedule.day.${d}` })}
              </button>
            );
          })}
        </div>
      )}

      {value.kind !== 'now' && <p className="text-xs text-muted-foreground">{t('hint')}</p>}
    </div>
  );
}
