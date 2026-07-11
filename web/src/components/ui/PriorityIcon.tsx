import { useIntl } from 'react-intl';
import { SignalLow, SignalMedium, SignalHigh, AlertTriangle, type LucideIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * PriorityIcon — the task-priority glyph (paperclip P2). Matches the backend
 * `TaskPriority` vocabulary. Non-interactive: it labels via title + aria.
 */
export type TaskPriorityKey = 'low' | 'medium' | 'high' | 'urgent';

const ICONS: Record<TaskPriorityKey, LucideIcon> = {
  low: SignalLow,
  medium: SignalMedium,
  high: SignalHigh,
  urgent: AlertTriangle,
};

// Priority reuses status-icon hues: low→grey, medium→amber, high→blue, urgent→red.
const COLORS: Record<TaskPriorityKey, string> = {
  low: 'var(--status-task-icon-backlog)',
  medium: 'var(--status-task-icon-todo)',
  high: 'var(--status-task-icon-in_progress)',
  urgent: 'var(--status-task-icon-blocked)',
};

const DEFAULT_LABELS: Record<TaskPriorityKey, string> = {
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  urgent: 'Urgent',
};

const SIZES = { sm: 14, md: 18, lg: 22 } as const;

export function PriorityIcon({
  priority,
  size = 'md',
  className,
}: {
  priority: TaskPriorityKey;
  size?: keyof typeof SIZES;
  className?: string;
}) {
  const intl = useIntl();
  const Icon = ICONS[priority];
  const px = SIZES[size];
  const label = intl.formatMessage({
    id: `taskPriority.${priority}`,
    defaultMessage: DEFAULT_LABELS[priority],
  });
  return (
    <span className={cn('inline-flex', className)} title={label} role="img" aria-label={label}>
      <Icon width={px} height={px} style={{ color: COLORS[priority] }} aria-hidden="true" className="shrink-0" />
    </span>
  );
}
