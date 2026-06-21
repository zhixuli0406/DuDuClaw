import { useId, type ReactNode, type ComponentType } from 'react';
import { cn } from '@/lib/utils';

export type TabItem = {
  id: string;
  label: ReactNode;
  icon?: ComponentType<{ className?: string }>;
  badge?: ReactNode;
};

/**
 * Tabs — accessible underline tab strip with roving arrow-key navigation.
 * Controlled: pass `value` + `onChange`. Render panels yourself via the
 * returned active id (keeps it data-source agnostic).
 */
export function Tabs({
  items,
  value,
  onChange,
  className,
}: {
  items: readonly TabItem[];
  value: string;
  onChange: (id: string) => void;
  className?: string;
}) {
  const groupId = useId();

  const onKeyDown = (e: React.KeyboardEvent, index: number) => {
    if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
    e.preventDefault();
    const dir = e.key === 'ArrowRight' ? 1 : -1;
    const next = (index + dir + items.length) % items.length;
    onChange(items[next].id);
    document.getElementById(`${groupId}-tab-${items[next].id}`)?.focus();
  };

  return (
    <div
      role="tablist"
      className={cn(
        'flex items-center gap-1 overflow-x-auto border-b border-[var(--panel-border)]',
        className
      )}
    >
      {items.map((item, i) => {
        const active = item.id === value;
        const Icon = item.icon;
        return (
          <button
            key={item.id}
            id={`${groupId}-tab-${item.id}`}
            role="tab"
            aria-selected={active}
            tabIndex={active ? 0 : -1}
            onClick={() => onChange(item.id)}
            onKeyDown={(e) => onKeyDown(e, i)}
            className={cn(
              'relative flex shrink-0 items-center gap-1.5 whitespace-nowrap px-3 py-2 text-sm font-medium transition-colors',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
              active
                ? 'text-amber-700 dark:text-amber-400'
                : 'text-stone-500 hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200'
            )}
          >
            {Icon && <Icon className="h-4 w-4" />}
            {item.label}
            {item.badge != null && (
              <span className="ml-1 rounded-full bg-stone-500/12 px-1.5 text-[11px] tabular-nums text-stone-500 dark:text-stone-400">
                {item.badge}
              </span>
            )}
            <span
              className={cn(
                'absolute inset-x-2 -bottom-px h-0.5 rounded-full bg-amber-500 transition-opacity dark:bg-amber-400',
                active ? 'opacity-100' : 'opacity-0'
              )}
              aria-hidden="true"
            />
          </button>
        );
      })}
    </div>
  );
}
