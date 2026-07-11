import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

export interface EntityTab {
  /** Stable key, also used in the URL/`:tab` param. */
  id: string;
  /** Already-resolved tab label. */
  label: string;
  /** Optional live count pill. */
  count?: number;
}

/**
 * EntityDetailShell — the tab-shell scaffold for entity detail pages
 * (paperclip P5, dashboard-redesign §8): a header (avatar + title + actions) and
 * a horizontal tab strip, with the body rendered by the caller. Used by AI-staff
 * detail (§5.3) and any future routine/entity detail. Presentational and
 * controlled: the caller owns the active tab + routing.
 */
export function EntityDetailShell({
  avatar,
  title,
  subtitle,
  actions,
  tabs,
  activeTab,
  onTabChange,
  children,
}: {
  avatar?: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  tabs: readonly EntityTab[];
  activeTab: string;
  onTabChange: (id: string) => void;
  children: ReactNode;
}) {
  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-center gap-3">
          {avatar && (
            <span className="grid h-12 w-12 shrink-0 place-items-center rounded-xl bg-stone-500/10 text-2xl dark:bg-white/5">
              {avatar}
            </span>
          )}
          <div className="min-w-0">
            <h1 className="truncate text-xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">{title}</h1>
            {subtitle && <p className="mt-0.5 truncate text-sm text-stone-500 dark:text-stone-400">{subtitle}</p>}
          </div>
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>

      <div role="tablist" className="flex gap-1 overflow-x-auto border-b border-[var(--panel-border)]">
        {tabs.map((tab) => {
          const isActive = tab.id === activeTab;
          return (
            <button
              key={tab.id}
              role="tab"
              aria-selected={isActive}
              onClick={() => onTabChange(tab.id)}
              className={cn(
                '-mb-px flex shrink-0 items-center gap-1.5 border-b-2 px-3.5 py-2 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                isActive
                  ? 'border-amber-500 text-amber-700 dark:border-amber-400 dark:text-amber-300'
                  : 'border-transparent text-stone-500 hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200',
              )}
            >
              {tab.label}
              {typeof tab.count === 'number' && tab.count > 0 && (
                <span className="rounded-full bg-stone-500/15 px-1.5 text-[10px] tabular-nums text-stone-600 dark:text-stone-300">
                  {tab.count}
                </span>
              )}
            </button>
          );
        })}
      </div>

      <div>{children}</div>
    </div>
  );
}
