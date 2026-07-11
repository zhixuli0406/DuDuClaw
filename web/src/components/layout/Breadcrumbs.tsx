import { Link } from 'react-router';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib/utils';

export interface Crumb {
  /** Already-resolved display label (caller translates via intl). */
  label: string;
  /** Optional link target; the last crumb is usually a plain label. */
  to?: string;
}

/**
 * Breadcrumbs — the horizontal trail that runs through the header and every
 * ManageShell page (paperclip P6). Presentational: the caller resolves labels
 * (i18n) and supplies the trail so this stays route-agnostic and testable.
 */
export function Breadcrumbs({ items, className }: { items: readonly Crumb[]; className?: string }) {
  if (items.length === 0) return null;
  return (
    <nav aria-label="breadcrumb" className={cn('flex min-w-0 items-center gap-1 text-sm', className)}>
      {items.map((crumb, i) => {
        const isLast = i === items.length - 1;
        return (
          <span key={`${crumb.label}-${i}`} className="flex min-w-0 items-center gap-1">
            {i > 0 && <ChevronRight className="h-3.5 w-3.5 shrink-0 text-stone-300 dark:text-stone-600" aria-hidden="true" />}
            {crumb.to && !isLast ? (
              <Link
                to={crumb.to}
                className="truncate text-stone-500 transition-colors hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200"
              >
                {crumb.label}
              </Link>
            ) : (
              <span
                className={cn(
                  'truncate',
                  isLast ? 'font-medium text-stone-800 dark:text-stone-100' : 'text-stone-500 dark:text-stone-400',
                )}
                aria-current={isLast ? 'page' : undefined}
              >
                {crumb.label}
              </span>
            )}
          </span>
        );
      })}
    </nav>
  );
}
