import type { ComponentType, ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * EmptyState — the consistent "nothing here yet" surface: icon, title, hint,
 * and an optional primary action. Use for empty lists, no-results, first-run.
 */
export function EmptyState({
  icon: Icon,
  title,
  hint,
  action,
  className,
}: {
  icon?: ComponentType<{ className?: string }>;
  title: ReactNode;
  hint?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center gap-3 px-6 py-12 text-center',
        className
      )}
    >
      {Icon && (
        <span className="grid h-12 w-12 place-items-center rounded-2xl bg-stone-500/8 text-stone-400 dark:bg-white/5">
          <Icon className="h-6 w-6" />
        </span>
      )}
      <div className="space-y-1">
        <p className="text-sm font-medium text-stone-700 dark:text-stone-200">{title}</p>
        {hint && (
          <p className="mx-auto max-w-sm text-xs text-stone-500 dark:text-stone-400">{hint}</p>
        )}
      </div>
      {action}
    </div>
  );
}
