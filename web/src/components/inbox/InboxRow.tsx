import { cn } from '@/lib/utils';
import { Badge, CharacterAvatar } from '@/components/ui';
import { timeAgo } from '@/lib/format';
import type { InboxColumn, InboxItem } from '@/lib/inbox-model';
import { SwipeToArchive } from '@/components/ui';
import { TYPE_META } from './meta';

export interface InboxRowLabels {
  typeLabel: (item: InboxItem) => string;
  approve: string;
  reject: string;
  view: string;
  archive: string;
}

export interface InboxRowProps {
  item: InboxItem;
  selected: boolean;
  columns: readonly InboxColumn[];
  /** `a` shortcut + swipe archive only on the "我的" tab. */
  canArchive: boolean;
  /** Display name for the leading avatar's a11y label. */
  agentName?: string;
  labels: InboxRowLabels;
  onSelect: () => void;
  onOpen: () => void;
  onApprove: () => void;
  onReject: () => void;
  onView: () => void;
  onArchive: () => void;
}

/** Single inbox row — leading staff avatar + badges + title + actions. */
export function InboxRow(props: InboxRowProps) {
  const { item, selected, columns, canArchive, agentName, labels } = props;
  const meta = TYPE_META[item.type];
  const Icon = meta.icon;
  const isApproval = item.type === 'approval';

  const row = (
    <div
      role="option"
      aria-selected={selected}
      onMouseEnter={props.onSelect}
      onClick={props.onOpen}
      className={cn(
        'panel flex cursor-pointer items-center gap-3 px-4 py-3 transition-colors',
        selected && 'ring-1 ring-inset ring-amber-500/50',
      )}
    >
      {/* Leading: originating staff avatar, or the type glyph when unowned. */}
      <span className="shrink-0">
        {item.agentId ? (
          <CharacterAvatar agentId={item.agentId} name={agentName ?? item.agentId} size={24} />
        ) : (
          <span className="grid h-6 w-6 place-items-center rounded-lg bg-stone-500/10 text-stone-500 dark:bg-white/5 dark:text-stone-400">
            <Icon className="h-3.5 w-3.5" />
          </span>
        )}
      </span>

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          {columns.includes('type') && <Badge tone={meta.tone}>{labels.typeLabel(item)}</Badge>}
          {columns.includes('agent') && item.agentId && (
            <span className="truncate text-xs text-stone-400 dark:text-stone-500">{agentName ?? item.agentId}</span>
          )}
          {columns.includes('channel') && item.channel && (
            <span className="truncate rounded bg-stone-500/10 px-1.5 py-0.5 text-[11px] text-stone-500 dark:bg-white/5 dark:text-stone-400">
              {item.channel}
            </span>
          )}
          {columns.includes('time') && (
            <span className="font-mono text-[11px] tabular-nums text-stone-400 dark:text-stone-500">
              {timeAgo(item.timestamp)}
            </span>
          )}
        </div>
        <p className="mt-0.5 truncate text-sm text-stone-800 dark:text-stone-100" title={item.title}>
          {item.title}
        </p>
      </div>

      <div className="flex shrink-0 items-center gap-1.5" onClick={(e) => e.stopPropagation()}>
        {isApproval ? (
          <>
            <button
              onClick={props.onApprove}
              className="rounded-control bg-amber-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-amber-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40"
            >
              {labels.approve}
            </button>
            <button
              onClick={props.onReject}
              className="rounded-control px-3 py-1.5 text-xs font-medium text-rose-600 transition-colors hover:bg-rose-500/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-rose-500/40 dark:text-rose-400"
            >
              {labels.reject}
            </button>
          </>
        ) : (
          item.actionable && (
            <button
              onClick={props.onView}
              className="rounded-control bg-amber-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-amber-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40"
            >
              {labels.view}
            </button>
          )
        )}
        {canArchive && (
          <button
            onClick={props.onArchive}
            title={labels.archive}
            aria-label={labels.archive}
            className="rounded-control px-3 py-1.5 text-xs font-medium text-stone-500 transition-colors hover:bg-stone-500/10 hover:text-stone-800 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200"
          >
            {labels.archive}
          </button>
        )}
      </div>
    </div>
  );

  // Mobile swipe-to-archive only where archiving is allowed.
  if (canArchive) {
    return (
      <SwipeToArchive onArchive={props.onArchive} className="rounded-card">
        {row}
      </SwipeToArchive>
    );
  }
  return row;
}
