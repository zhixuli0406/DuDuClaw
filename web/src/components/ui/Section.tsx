import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Section — a labeled block: small heading + optional description and a
 * right-aligned actions slot, followed by children. Use to group related
 * cards/controls on a page without nesting another surface.
 */
export function Section({
  title,
  description,
  actions,
  children,
  className,
}: {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn('space-y-3', className)}>
      {(title || actions) && (
        <div className="flex items-end justify-between gap-3">
          <div className="min-w-0">
            {title && (
              <h2 className="text-sm font-semibold text-stone-800 dark:text-stone-100">
                {title}
              </h2>
            )}
            {description && (
              <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                {description}
              </p>
            )}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </div>
      )}
      {children}
    </section>
  );
}
