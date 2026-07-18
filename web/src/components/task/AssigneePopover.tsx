import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Check } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ActorAvatar } from '@/components/mds';

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
  allowUnassigned = false,
}: {
  agents: ReadonlyArray<AssigneeOption>;
  value: string | null;
  onChange: (agentName: string) => void;
  children?: ReactNode;
  align?: 'left' | 'right';
  className?: string;
  /** Offer an explicit "unassigned" choice (empty string) at the top of the
   *  list and label the unselected trigger as "unassigned" rather than a
   *  call-to-action. Used by the create-task modal so a task can stay
   *  unassigned instead of being force-assigned to the first agent (Bug#4). */
  allowUnassigned?: boolean;
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
          <ActorAvatar actorType="agent" size="sm" name={selected.display_name} />
          <span className="truncate text-sm text-foreground">{selected.display_name}</span>
        </>
      ) : (
        <span className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: allowUnassigned ? 'tasks.assignee.unassigned' : 'tasks.assign' })}
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
        className="inline-flex items-center gap-1.5 rounded-lg px-1.5 py-1 outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        {trigger}
      </button>
      {open && (
        <div
          id={menuId}
          role="menu"
          className={cn(
            'absolute top-full z-50 mt-1 max-h-64 min-w-52 overflow-y-auto rounded-lg bg-surface-raised p-1 shadow-[var(--menu-shadow)] ring-1 ring-surface-border',
            align === 'right' ? 'right-0' : 'left-0',
          )}
        >
          {allowUnassigned && (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={!value}
              onClick={() => {
                onChange('');
                setOpen(false);
              }}
              className={cn(
                'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground',
                !value && 'font-medium',
              )}
            >
              <span className="grid size-6 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground">—</span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {intl.formatMessage({ id: 'tasks.assignee.unassigned' })}
              </span>
              {!value && <Check className="size-3.5 shrink-0 text-brand" />}
            </button>
          )}
          {agents.length === 0 && !allowUnassigned && (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
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
                'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground',
                a.name === value && 'font-medium',
              )}
            >
              <ActorAvatar actorType="agent" size="sm" name={a.display_name} />
              <span className="min-w-0 flex-1 truncate text-foreground">{a.display_name}</span>
              {a.name === value && <Check className="size-3.5 shrink-0 text-brand" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
