import { useEffect, useId, useRef, useState, type ComponentType, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { CircleDot, Flag, UserRound, CalendarPlus, CalendarClock, CheckCircle2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  StatusIcon,
  PriorityIcon,
  useStatusLabel,
  type TaskPriorityKey,
} from '@/components/ui';
import { toStatusKey, toBackendStatus } from '@/lib/task-status';
import { AssigneePopover, type AssigneeOption } from './AssigneePopover';
import type { TaskInfo, TaskPriority } from '@/lib/api';

const PRIORITY_ORDER: readonly TaskPriorityKey[] = ['low', 'medium', 'high', 'urgent'];

/** A grouped section of property rows (Multica PropertyRow style, spec §5.3). */
function PropSection({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-0.5">
      <h3 className="px-1 pb-1 text-xs font-medium text-muted-foreground">{title}</h3>
      <div className="space-y-0.5">{children}</div>
    </section>
  );
}

/** One label-left / value-right line (`text-sm`, label muted). */
function PropRow({
  label,
  icon: Icon,
  children,
}: {
  label: ReactNode;
  icon?: ComponentType<{ className?: string }>;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 px-1 py-1.5 text-sm">
      <span className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground">
        {Icon && <Icon className="size-3.5 shrink-0" />}
        {label}
      </span>
      <span className="ml-auto flex min-w-0 items-center justify-end gap-1.5 text-right text-foreground">
        {children}
      </span>
    </div>
  );
}

/** A tiny popover to change task priority (PriorityIcon itself is display-only). */
function PriorityPopover({
  value,
  onChange,
}: {
  value: TaskPriorityKey;
  onChange: (p: TaskPriority) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const menuId = useId();

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('mousedown', onDoc);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDoc);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative inline-flex">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={() => setOpen((v) => !v)}
        className="inline-flex items-center gap-1.5 rounded-lg px-1.5 py-1 outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <PriorityIcon priority={value} size="sm" />
        <span className="text-sm text-foreground">
          {intl.formatMessage({ id: `tasks.priority.${value}` })}
        </span>
      </button>
      {open && (
        <div
          id={menuId}
          role="menu"
          className="absolute right-0 top-full z-50 mt-1 min-w-40 rounded-lg bg-surface-raised p-1 shadow-[var(--menu-shadow)] ring-1 ring-surface-border"
        >
          {PRIORITY_ORDER.map((p) => (
            <button
              key={p}
              type="button"
              role="menuitemradio"
              aria-checked={p === value}
              onClick={() => {
                onChange(p);
                setOpen(false);
              }}
              className={cn(
                'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground',
                p === value && 'font-medium',
              )}
            >
              <PriorityIcon priority={p} size="sm" />
              <span className="text-foreground">
                {intl.formatMessage({ id: `tasks.priority.${p}` })}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * TaskProperties — the right-hand PropertiesPanel content for the detail page
 * (spec §5.3 式1): Triage (status / priority / assignee) + About (timestamps).
 * All edits write straight back through the callbacks the page threads from the
 * tasks store. Status writes go through the mapping layer so a forward-looking
 * pick (backlog/in_review/cancelled) is ignored rather than mis-persisted.
 */
export function TaskProperties({
  task,
  agents,
  onStatusChange,
  onPriorityChange,
  onAssign,
}: {
  task: TaskInfo;
  agents: ReadonlyArray<AssigneeOption>;
  onStatusChange: (next: import('@/lib/api').TaskStatus) => void;
  onPriorityChange: (next: TaskPriority) => void;
  onAssign: (agentName: string) => void;
}) {
  const intl = useIntl();
  const statusLabel = useStatusLabel();
  const statusKey = toStatusKey(task.status);

  const fmt = (d?: string) =>
    d
      ? new Date(d).toLocaleString(intl.locale, {
          year: 'numeric',
          month: 'short',
          day: 'numeric',
          hour: '2-digit',
          minute: '2-digit',
        })
      : '—';

  return (
    <div className="space-y-4">
      <PropSection title={intl.formatMessage({ id: 'tasks.props.triage' })}>
        <PropRow label={intl.formatMessage({ id: 'tasks.field.status' })} icon={CircleDot}>
          <StatusIcon
            status={statusKey}
            size="sm"
            onChange={(next) => {
              const backend = toBackendStatus(next);
              if (backend && backend !== task.status) onStatusChange(backend);
            }}
          />
          <span className="text-sm text-foreground">{statusLabel(statusKey)}</span>
        </PropRow>
        <PropRow label={intl.formatMessage({ id: 'tasks.field.priority' })} icon={Flag}>
          <PriorityPopover value={task.priority} onChange={onPriorityChange} />
        </PropRow>
        <PropRow label={intl.formatMessage({ id: 'tasks.field.assignTo' })} icon={UserRound}>
          <AssigneePopover agents={agents} value={task.assigned_to || null} onChange={onAssign} align="right" />
        </PropRow>
      </PropSection>

      <PropSection title={intl.formatMessage({ id: 'tasks.props.about' })}>
        <PropRow label={intl.formatMessage({ id: 'tasks.detail.createdBy' })} icon={UserRound}>
          <span className="truncate text-sm text-foreground">{task.created_by}</span>
        </PropRow>
        <PropRow label={intl.formatMessage({ id: 'tasks.detail.createdAt' })} icon={CalendarPlus}>
          <span className="font-mono text-xs tabular-nums text-muted-foreground">{fmt(task.created_at)}</span>
        </PropRow>
        <PropRow label={intl.formatMessage({ id: 'tasks.detail.updatedAt' })} icon={CalendarClock}>
          <span className="font-mono text-xs tabular-nums text-muted-foreground">{fmt(task.updated_at)}</span>
        </PropRow>
        {task.completed_at && (
          <PropRow label={intl.formatMessage({ id: 'tasks.detail.completedAt' })} icon={CheckCircle2}>
            <span className="font-mono text-xs tabular-nums text-muted-foreground">{fmt(task.completed_at)}</span>
          </PropRow>
        )}
      </PropSection>
    </div>
  );
}
