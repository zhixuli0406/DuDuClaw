import type { ReactNode } from 'react';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * GroupHeader — a collapsible "label · count" row heading grouped lists
 * (inbox group-by, blocked tri-buckets, task group-by). Chevron rotates open;
 * the whole row is the toggle button for a big hit target.
 */
export function GroupHeader({
  label,
  count,
  collapsed = false,
  onToggle,
  actions,
  className,
}: {
  label: ReactNode;
  count?: number;
  collapsed?: boolean;
  onToggle?: () => void;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex items-center gap-2', className)}>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={!collapsed}
        className="group flex min-w-0 flex-1 items-center gap-1.5 rounded-xl py-1 text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <ChevronRight
          className={cn(
            'h-4 w-4 shrink-0 text-muted-foreground transition-transform',
            !collapsed && 'rotate-90',
          )}
          aria-hidden="true"
        />
        <span className="truncate text-sm font-semibold text-foreground">
          {label}
        </span>
        {typeof count === 'number' && (
          <span className="ml-0.5 rounded-full bg-muted px-1.5 text-xs font-medium tabular-nums text-muted-foreground">
            {count}
          </span>
        )}
      </button>
      {actions && <div className="flex shrink-0 items-center gap-1">{actions}</div>}
    </div>
  );
}
