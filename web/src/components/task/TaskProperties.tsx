import { useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { CircleDot, Flag, UserRound, CalendarPlus, CalendarClock, CheckCircle2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  PropertySection,
  PropertyRow,
  StatusIcon,
  PriorityIcon,
  useStatusLabel,
  Mono,
  type TaskPriorityKey,
} from '@/components/ui';
import { toStatusKey, toBackendStatus } from '@/lib/task-status';
import { AssigneePopover, type AssigneeOption } from './AssigneePopover';
import type { TaskInfo, TaskPriority } from '@/lib/api';

const PRIORITY_ORDER: readonly TaskPriorityKey[] = ['low', 'medium', 'high', 'urgent'];

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
        className="inline-flex items-center gap-1.5 rounded-control px-1.5 py-1 hover:bg-stone-500/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:hover:bg-white/5"
      >
        <PriorityIcon priority={value} size="sm" />
        <span className="text-sm text-stone-700 dark:text-stone-200">
          {intl.formatMessage({ id: `tasks.priority.${value}` })}
        </span>
      </button>
      {open && (
        <div id={menuId} role="menu" className="glass-overlay absolute right-0 top-full z-50 mt-1 min-w-40 rounded-control p-1">
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
                'flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm hover:bg-stone-500/10 dark:hover:bg-white/5',
                p === value && 'font-semibold',
              )}
            >
              <PriorityIcon priority={p} size="sm" />
              <span className="text-stone-700 dark:text-stone-200">
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
 * (§5.3 T5.3): Triage (status / priority / assignee) + About (timestamps). All
 * edits write straight back through the callbacks the page threads from the
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
      <PropertySection title={intl.formatMessage({ id: 'tasks.props.triage' })}>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.field.status' })} icon={CircleDot}>
          <StatusIcon
            status={statusKey}
            size="sm"
            onChange={(next) => {
              const backend = toBackendStatus(next);
              if (backend && backend !== task.status) onStatusChange(backend);
            }}
          />
          <span className="text-sm text-stone-700 dark:text-stone-200">{statusLabel(statusKey)}</span>
        </PropertyRow>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.field.priority' })} icon={Flag}>
          <PriorityPopover value={task.priority} onChange={onPriorityChange} />
        </PropertyRow>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.field.assignTo' })} icon={UserRound}>
          <AssigneePopover agents={agents} value={task.assigned_to || null} onChange={onAssign} align="right" />
        </PropertyRow>
      </PropertySection>

      <PropertySection title={intl.formatMessage({ id: 'tasks.props.about' })}>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.detail.createdBy' })} icon={UserRound}>
          <span className="truncate text-sm text-stone-700 dark:text-stone-200">{task.created_by}</span>
        </PropertyRow>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.detail.createdAt' })} icon={CalendarPlus}>
          <Mono className="text-xs">{fmt(task.created_at)}</Mono>
        </PropertyRow>
        <PropertyRow label={intl.formatMessage({ id: 'tasks.detail.updatedAt' })} icon={CalendarClock}>
          <Mono className="text-xs">{fmt(task.updated_at)}</Mono>
        </PropertyRow>
        {task.completed_at && (
          <PropertyRow label={intl.formatMessage({ id: 'tasks.detail.completedAt' })} icon={CheckCircle2}>
            <Mono className="text-xs">{fmt(task.completed_at)}</Mono>
          </PropertyRow>
        )}
      </PropertySection>
    </div>
  );
}
