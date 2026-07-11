import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * `<Mono>` — the wrapper for machine values (ids, cost, tokens, timestamps).
 * Renders in the mono font with tabular numerals so columns of values align
 * (dashboard-redesign §8, paperclip P8). Pair with `lib/format.ts` helpers.
 */
export function Mono({
  children,
  className,
  title,
}: {
  children: ReactNode;
  className?: string;
  title?: string;
}) {
  return (
    <span
      title={title}
      className={cn('font-mono text-[0.8125rem] tabular-nums text-stone-600 dark:text-stone-400', className)}
    >
      {children}
    </span>
  );
}
