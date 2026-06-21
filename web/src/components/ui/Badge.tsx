import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

type Tone = 'neutral' | 'success' | 'warning' | 'danger' | 'info' | 'accent';

const tones: Record<Tone, string> = {
  neutral:
    'bg-stone-500/10 text-stone-600 ring-stone-500/20 dark:text-stone-300',
  success:
    'bg-emerald-500/12 text-emerald-700 ring-emerald-500/25 dark:text-emerald-400',
  warning:
    'bg-amber-500/12 text-amber-700 ring-amber-500/25 dark:text-amber-400',
  danger: 'bg-rose-500/12 text-rose-700 ring-rose-500/25 dark:text-rose-400',
  info: 'bg-sky-500/12 text-sky-700 ring-sky-500/25 dark:text-sky-400',
  accent:
    'bg-amber-500/15 text-amber-800 ring-amber-500/30 dark:text-amber-300',
};

/** Badge — small status pill. Dot variant for live/inline status indicators. */
export function Badge({
  children,
  tone = 'neutral',
  dot = false,
  className,
}: {
  children: ReactNode;
  tone?: Tone;
  dot?: boolean;
  className?: string;
}) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ring-1 ring-inset',
        tones[tone],
        className
      )}
    >
      {dot && (
        <span className="h-1.5 w-1.5 rounded-full bg-current opacity-80" aria-hidden="true" />
      )}
      {children}
    </span>
  );
}
