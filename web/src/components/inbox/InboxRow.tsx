import { Archive, CheckCircle2, Hourglass } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ActorAvatar } from '@/components/mds';
import { timeAgo, timeRemaining } from '@/lib/format';
import { expiryState, type InboxItem } from '@/lib/inbox-model';
import { useNowTick } from '@/hooks/useNowTick';
import type { RiskLevel } from '@/lib/approval-risk';
import { TYPE_META } from './meta';

export interface InboxRowLabels {
  typeLabel: (item: InboxItem) => string;
  /** Whole-action risk band → short label ("低/中/高"). */
  riskLabel: (level: RiskLevel) => string;
  archive: string;
  /** Short amber-marker label shown once an approval's TTL has burned
   *  through two-thirds of its window ("即將逾時"). */
  nearExpiry: string;
  /** Full explanation of what happens at the deadline, rendered as the
   *  countdown's hover tooltip ("逾時未決，將自動拒絕"). */
  nearExpiryTooltip: string;
  /** aria-label / tooltip for the "已處理" checkmark badge (§C6 — a second,
   *  independent axis from read/unread; a processed row is never hidden
   *  outright, only dimmed and sunk). */
  processedTooltip: string;
}

export interface InboxRowProps {
  item: InboxItem;
  selected: boolean;
  /** Renders the leading unread dot + heavier title weight. */
  unread: boolean;
  /** §C6: the user has already resolved this item. Dims the row and shows a
   *  checkmark badge — never hides it (that's `archived`, a separate axis). */
  processed: boolean;
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
  const { item, selected, unread, processed, canArchive, agentName, labels } = props;
  const meta = TYPE_META[item.type];
  const Icon = meta.icon;

  // Live TTL countdown (approvals + installs — both are two-stage-signable
  // decisions with a server-computed deadline) — ticks only while this row
  // actually has a deadline to track, so every other row costs nothing.
  const tracksExpiry = (item.type === 'approval' || item.type === 'install') && item.expiresAt != null;
  const now = useNowTick(tracksExpiry);
  const expiry = tracksExpiry ? expiryState(item, now) : null;

  return (
    <div
      role="option"
      aria-selected={selected}
      onMouseEnter={props.onSelect}
      onClick={props.onSelect}
      className={cn(
        'group/row flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 transition-colors',
        selected ? 'bg-surface-selected' : 'hover:bg-surface-hover',
        // Sunk + dimmed, never hidden — the row stays reachable (§C6).
        processed && !selected && 'opacity-55',
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
          {/* TTL countdown — always shown once an approval carries an expiry;
              the amber "即將逾時" marker + tint join in once under a third of
              the window remains. Tooltip spells out that timing out counts
              as an automatic rejection (never a silent no-op). */}
          {expiry && (
            <span
              title={labels.nearExpiryTooltip}
              className={cn(
                'inline-flex shrink-0 items-center gap-1 tabular-nums',
                expiry.nearExpiry && 'rounded bg-warning/15 px-1 py-0.5 font-medium text-warning',
              )}
            >
              <Hourglass className="size-2.5 shrink-0" aria-hidden="true" />
              {expiry.nearExpiry && <span>{labels.nearExpiry}</span>}
              <span>{timeRemaining(expiry.remainingMs)}</span>
            </span>
          )}
          {item.channel && (
            <span className="truncate rounded bg-muted px-1 text-[10px] font-medium">{item.channel}</span>
          )}
          {processed && (
            <span title={labels.processedTooltip} aria-label={labels.processedTooltip} className="inline-flex shrink-0">
              <CheckCircle2 className="size-3 shrink-0 text-success" aria-hidden="true" />
            </span>
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
