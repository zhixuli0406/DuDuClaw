import type { ReactNode } from 'react';
import { Info, ShieldAlert, AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';

type Tone = 'danger' | 'warning' | 'info';

const TONE_CLASS: Record<Tone, string> = {
  danger: 'border-destructive/30 bg-destructive/5 text-destructive',
  // Amber text uses the 700 / 400 stops (web/DESIGN.md: amber-500 is decorative only).
  warning: 'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400',
  info: 'border-border bg-muted/40 text-muted-foreground',
};

const TONE_ICON: Record<Tone, typeof Info> = {
  danger: ShieldAlert,
  warning: AlertTriangle,
  info: Info,
};

/**
 * An inline notice. `danger` notices are announced assertively
 * (`role="alert"`); the others politely (`role="status"`), so a state that
 * appears after a poll reaches screen-reader users without stealing focus.
 */
export function Notice({ tone, children, className }: { tone: Tone; children: ReactNode; className?: string }) {
  const Icon = TONE_ICON[tone];
  return (
    <div
      role={tone === 'danger' ? 'alert' : 'status'}
      className={cn('flex items-start gap-2 rounded-lg border px-3 py-2.5 text-sm', TONE_CLASS[tone], className)}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div className="min-w-0 space-y-1">{children}</div>
    </div>
  );
}
