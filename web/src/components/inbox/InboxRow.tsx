import { Archive } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ActorAvatar } from '@/components/mds';
import { timeAgo } from '@/lib/format';
import type { InboxItem } from '@/lib/inbox-model';
import type { RiskLevel } from '@/lib/approval-risk';
import { TYPE_META } from './meta';

export interface InboxRowLabels {
  typeLabel: (item: InboxItem) => string;
  /** Whole-action risk band → short label ("低/中/高"). */
  riskLabel: (level: RiskLevel) => string;
  archive: string;
}

export interface InboxRowProps {
  item: InboxItem;
  selected: boolean;
  /** Renders the leading unread dot + heavier title weight. */
  unread: boolean;
  /** Hover archive button only on the "我的" tab. */
  canArchive: boolean;
  /** Display name for the leading avatar. */
  agentName?: string;
  labels: InboxRowLabels;
  onSelect: () => void;
  onArchive: () => void;
}

/** Risk band → dot colour token. */
function riskDot(level: RiskLevel): string {
  return level === 'high' ? 'bg-destructive' : level === 'medium' ? 'bg-warning' : 'bg-success';
}

/**
 * InboxRow — the slim Multica list row (spec §5.6): leading ActorAvatar, a
 * truncating title, a relative timestamp, and an unread `bg-brand` dot. Actions
 * (approve / reject / view) live in the right-hand detail panel, not the row —
 * selecting a row opens it there. Archive is a hover-only affordance.
 */
export function InboxRow(props: InboxRowProps) {
  const { item, selected, unread, canArchive, agentName, labels } = props;
  const meta = TYPE_META[item.type];
  const Icon = meta.icon;

  return (
    <div
      role="option"
      aria-selected={selected}
      onMouseEnter={props.onSelect}
      onClick={props.onSelect}
      className={cn(
        'group/row flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 transition-colors',
        selected ? 'bg-surface-selected' : 'hover:bg-surface-hover',
      )}
    >
      {/* Leading: originating staff avatar, or the type glyph when unowned. */}
      {item.agentId ? (
        <ActorAvatar actorType="agent" size="sm" name={agentName ?? item.agentId} className="shrink-0" />
      ) : (
        <span className="grid size-5 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border">
          <Icon className="size-3" />
        </span>
      )}

      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <p
            className={cn(
              'min-w-0 flex-1 truncate text-sm',
              unread ? 'font-medium text-foreground' : 'text-foreground/90',
            )}
            title={item.title}
          >
            {item.title}
          </p>
          {unread && <span className="size-1.5 shrink-0 rounded-full bg-brand" aria-label="unread" />}
          <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
            {timeAgo(item.timestamp)}
          </span>
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
          <span className="truncate">{labels.typeLabel(item)}</span>
          {item.type === 'approval' && item.risk && (
            <span className="inline-flex shrink-0 items-center gap-1">
              <span className={cn('size-1.5 rounded-full', riskDot(item.risk))} aria-hidden="true" />
              {labels.riskLabel(item.risk)}
            </span>
          )}
          {item.channel && (
            <span className="truncate rounded bg-muted px-1 text-[10px] font-medium">{item.channel}</span>
          )}
        </div>
      </div>

      {canArchive && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            props.onArchive();
          }}
          title={labels.archive}
          aria-label={labels.archive}
          className="shrink-0 rounded-md p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-surface-hover hover:text-foreground focus-visible:opacity-100 focus-visible:ring-3 focus-visible:ring-ring/50 group-hover/row:opacity-100 pointer-coarse:opacity-100"
        >
          <Archive className="size-3.5" />
        </button>
      )}
    </div>
  );
}
