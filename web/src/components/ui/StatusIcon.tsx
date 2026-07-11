import { useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  CircleDashed,
  Circle,
  CircleDot,
  Eye,
  CheckCircle2,
  Ban,
  XCircle,
  type LucideIcon,
} from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * StatusIcon — the one task-status glyph used everywhere a task appears
 * (list rows, detail header, board cards). Renders a coloured circle icon in
 * the AA-contrast `--status-task-icon-*` token, and — when `onChange` is given
 * — opens a small popover to reassign the status inline (paperclip P2/P6).
 *
 * The vocabulary is the full 7-state design set (a superset of the current
 * backend `TaskStatus`); pages map their data onto it.
 */
export type TaskStatusKey =
  | 'backlog'
  | 'todo'
  | 'in_progress'
  | 'in_review'
  | 'done'
  | 'blocked'
  | 'cancelled';

export const TASK_STATUS_ORDER: readonly TaskStatusKey[] = [
  'backlog',
  'todo',
  'in_progress',
  'in_review',
  'done',
  'blocked',
  'cancelled',
];

const ICONS: Record<TaskStatusKey, LucideIcon> = {
  backlog: CircleDashed,
  todo: Circle,
  in_progress: CircleDot,
  in_review: Eye,
  done: CheckCircle2,
  blocked: Ban,
  cancelled: XCircle,
};

const DEFAULT_LABELS: Record<TaskStatusKey, string> = {
  backlog: 'Backlog',
  todo: 'To do',
  in_progress: 'In progress',
  in_review: 'In review',
  done: 'Done',
  blocked: 'Blocked',
  cancelled: 'Cancelled',
};

/** Resolve a translated status label (falls back to the built-in English). */
export function useStatusLabel(): (s: TaskStatusKey) => string {
  const intl = useIntl();
  return (s) =>
    intl.formatMessage({ id: `taskStatus.${s}`, defaultMessage: DEFAULT_LABELS[s] });
}

const SIZES = { sm: 14, md: 18, lg: 22 } as const;

export function StatusIcon({
  status,
  onChange,
  size = 'md',
  className,
}: {
  status: TaskStatusKey;
  /** When provided the icon becomes a button that opens a status-picker popover. */
  onChange?: (next: TaskStatusKey) => void;
  size?: keyof typeof SIZES;
  className?: string;
}) {
  const label = useStatusLabel();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const Icon = ICONS[status];
  const px = SIZES[size];
  const color = `var(--status-task-icon-${status})`;

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

  const glyph = (
    <Icon width={px} height={px} style={{ color }} aria-hidden="true" className="shrink-0" />
  );

  if (!onChange) {
    return (
      <span className={cn('inline-flex', className)} title={label(status)} role="img" aria-label={label(status)}>
        {glyph}
      </span>
    );
  }

  return (
    <div ref={rootRef} className={cn('relative inline-flex', className)}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={label(status)}
        title={label(status)}
        onClick={() => setOpen((v) => !v)}
        className="inline-flex items-center justify-center rounded-control p-0.5 hover:bg-stone-500/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:hover:bg-white/5"
      >
        {glyph}
      </button>
      {open && (
        <div
          id={menuId}
          role="menu"
          className="glass-overlay absolute left-0 top-full z-50 mt-1 min-w-40 rounded-control p-1"
        >
          {TASK_STATUS_ORDER.map((s) => {
            const RowIcon = ICONS[s];
            return (
              <button
                key={s}
                type="button"
                role="menuitemradio"
                aria-checked={s === status}
                onClick={() => {
                  onChange(s);
                  setOpen(false);
                }}
                className={cn(
                  'flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm hover:bg-stone-500/10 dark:hover:bg-white/5',
                  s === status && 'font-semibold',
                )}
              >
                <RowIcon
                  width={16}
                  height={16}
                  style={{ color: `var(--status-task-icon-${s})` }}
                  aria-hidden="true"
                />
                <span className="text-stone-700 dark:text-stone-200">{label(s)}</span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
