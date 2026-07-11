import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Check } from 'lucide-react';
import { cn } from '@/lib/utils';
import { CharacterAvatar } from '@/components/ui';

export interface AssigneeOption {
  name: string;
  display_name: string;
  status?: 'active' | 'paused' | 'terminated';
}

/**
 * AssigneePopover — pick an AI staff member from a small avatar list. Reused by
 * the task list rows, the create-task modal, and the detail-page properties
 * panel so "who owns this" always looks identical (character avatar + name).
 *
 * Controlled: pass `value` (agent name) + `onChange`. `children` is the trigger
 * content; if omitted a default avatar+name chip renders.
 */
export function AssigneePopover({
  agents,
  value,
  onChange,
  children,
  align = 'left',
  className,
}: {
  agents: ReadonlyArray<AssigneeOption>;
  value: string | null;
  onChange: (agentName: string) => void;
  children?: ReactNode;
  align?: 'left' | 'right';
  className?: string;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const selected = agents.find((a) => a.name === value) ?? null;

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDoc);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const trigger = children ?? (
    <span className="inline-flex items-center gap-1.5">
      {selected ? (
        <>
          <CharacterAvatar agentId={selected.name} name={selected.display_name} size={20} animated={false} />
          <span className="truncate text-sm text-stone-700 dark:text-stone-200">{selected.display_name}</span>
        </>
      ) : (
        <span className="text-sm text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'tasks.assign' })}
        </span>
      )}
    </span>
  );

  return (
    <div ref={rootRef} className={cn('relative inline-flex', className)}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={() => setOpen((v) => !v)}
        className="inline-flex items-center gap-1.5 rounded-control px-1.5 py-1 hover:bg-stone-500/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:hover:bg-white/5"
      >
        {trigger}
      </button>
      {open && (
        <div
          id={menuId}
          role="menu"
          className={cn(
            'glass-overlay absolute top-full z-50 mt-1 max-h-64 min-w-52 overflow-y-auto rounded-control p-1',
            align === 'right' ? 'right-0' : 'left-0',
          )}
        >
          {agents.length === 0 && (
            <p className="px-2 py-1.5 text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'tasks.assignee.empty' })}
            </p>
          )}
          {agents.map((a) => (
            <button
              key={a.name}
              type="button"
              role="menuitemradio"
              aria-checked={a.name === value}
              onClick={() => {
                onChange(a.name);
                setOpen(false);
              }}
              className={cn(
                'flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm hover:bg-stone-500/10 dark:hover:bg-white/5',
                a.name === value && 'font-semibold',
              )}
            >
              <CharacterAvatar agentId={a.name} name={a.display_name} size={22} animated={false} />
              <span className="min-w-0 flex-1 truncate text-stone-700 dark:text-stone-200">{a.display_name}</span>
              {a.name === value && <Check className="h-3.5 w-3.5 shrink-0 text-amber-600 dark:text-amber-400" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
