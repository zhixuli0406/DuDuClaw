import type { ReactNode } from 'react';
import { Search } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * Toolbar — the row above a list/table: a search box on the left and filters /
 * actions on the right. Search is optional (omit `onSearchChange`).
 */
export function Toolbar({
  search,
  onSearchChange,
  onSearchEnter,
  searchPlaceholder,
  children,
  className,
}: {
  search?: string;
  onSearchChange?: (v: string) => void;
  /** Fired when Enter is pressed in the search box (e.g. submit-on-enter). */
  onSearchEnter?: () => void;
  searchPlaceholder?: string;
  children?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex flex-wrap items-center gap-2', className)}>
      {onSearchChange && (
        <div className="relative min-w-[12rem] flex-1">
          <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="search"
            aria-label={searchPlaceholder ?? 'Search'}
            value={search ?? ''}
            onChange={(e) => onSearchChange(e.target.value)}
            onKeyDown={
              onSearchEnter
                ? (e) => {
                    if (e.key === 'Enter') onSearchEnter();
                  }
                : undefined
            }
            placeholder={searchPlaceholder}
            className="h-9 w-full rounded-lg border border-[var(--panel-border)] bg-[var(--panel-fill)] pl-9 pr-3 text-sm text-stone-800 placeholder:text-stone-400 focus-visible:border-amber-500/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/30 dark:text-stone-100"
          />
        </div>
      )}
      {children && <div className="flex items-center gap-2">{children}</div>}
    </div>
  );
}
