import type { ComponentType, ReactNode } from 'react';
import { cn } from '@/lib/utils';

type Tone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger';

const iconTones: Record<Tone, string> = {
  neutral: 'bg-stone-500/10 text-stone-500 dark:text-stone-400',
  accent: 'bg-amber-500/12 text-amber-600 dark:text-amber-400',
  success: 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-400',
  warning: 'bg-amber-500/12 text-amber-600 dark:text-amber-400',
  danger: 'bg-rose-500/12 text-rose-600 dark:text-rose-400',
};

/**
 * StatCard — a single metric tile: label, big value, optional delta and icon.
 * The hero KPI surface for Dashboard / Reports / Billing.
 */
export function StatCard({
  label,
  value,
  delta,
  deltaTone = 'neutral',
  icon: Icon,
  tone = 'neutral',
  hint,
  className,
}: {
  label: ReactNode;
  value: ReactNode;
  delta?: ReactNode;
  deltaTone?: 'up' | 'down' | 'neutral';
  icon?: ComponentType<{ className?: string }>;
  tone?: Tone;
  hint?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('panel p-4', className)}>
      <div className="flex items-start justify-between gap-3">
        <p className="text-xs font-medium text-stone-500 dark:text-stone-400">{label}</p>
        {Icon && (
          <span className={cn('grid h-8 w-8 place-items-center rounded-lg', iconTones[tone])}>
            <Icon className="h-4 w-4" />
          </span>
        )}
      </div>
      <div className="mt-2 flex items-baseline gap-2">
        <span className="text-2xl font-semibold tracking-tight tabular-nums text-stone-900 dark:text-stone-50">
          {value}
        </span>
        {delta != null && (
          <span
            className={cn(
              'text-xs font-medium tabular-nums',
              deltaTone === 'up' && 'text-emerald-600 dark:text-emerald-400',
              deltaTone === 'down' && 'text-rose-600 dark:text-rose-400',
              deltaTone === 'neutral' && 'text-stone-400'
            )}
          >
            {delta}
          </span>
        )}
      </div>
      {hint && <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">{hint}</p>}
    </div>
  );
}
