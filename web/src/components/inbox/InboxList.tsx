import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { InboxItem } from '@/lib/inbox-model';
import { InboxRow, type InboxRowLabels } from './InboxRow';

/** A precomputed group the page hands down (label omitted ⇒ no header row). */
export interface InboxGroup {
  key: string;
  label?: ReactNode;
  hint?: ReactNode;
  items: InboxItem[];
}

interface InboxListProps {
  groups: readonly InboxGroup[];
  canArchive: boolean;
  agentName: (id: string) => string;
  labels: InboxRowLabels;
  /** Currently-open item id (drives the highlighted row + keyboard cursor). */
  selectedId: string | null;
  /** Per-item unread flag (renders the leading dot). */
  isUnread: (item: InboxItem) => boolean;
  emptyState: ReactNode;
  /** Select a row → the page opens it in the detail pane + marks it read. */
  onSelect: (item: InboxItem) => void;
  onArchive: (item: InboxItem) => void;
  onUnread: (item: InboxItem) => void;
  onUndo: () => void;
}

type Entry =
  | { kind: 'header'; key: string; groupKey: string; label: ReactNode; hint?: ReactNode; count: number }
  | { kind: 'row'; key: string; groupKey: string; item: InboxItem };

function isEditableTarget(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
}

/** Plain Multica group header — chevron + label + count, collapses the group. */
function GroupHeaderRow({
  label,
  hint,
  count,
  collapsed,
  onToggle,
}: {
  label: ReactNode;
  hint?: ReactNode;
  count: number;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <div className="pt-2">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={!collapsed}
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left transition-colors hover:bg-surface-hover"
      >
        <ChevronDown className={cn('size-3.5 shrink-0 text-muted-foreground transition-transform', collapsed && '-rotate-90')} />
        <span className="truncate text-xs font-medium text-muted-foreground">{label}</span>
        <span className="font-mono text-xs tabular-nums text-muted-foreground/70">{count}</span>
      </button>
      {hint && !collapsed && <p className="px-2 pb-1 pl-7 text-xs text-muted-foreground/70">{hint}</p>}
    </div>
  );
}

/**
 * InboxList — renders precomputed groups and owns the keyboard flow (§5.6):
 * `j`/`k` (or ↑/↓) move between rows and open each in the detail pane, `←`/`→`
 * collapse/expand the current row's group, `Enter` re-opens, `a` archives (when
 * allowed), `U` marks unread, `⌘Z` undoes. Selection is controlled by the page
 * via `selectedId` so the split's detail pane stays in sync.
 */
export function InboxList(props: InboxListProps) {
  const { groups, canArchive, agentName, labels, selectedId, isUnread, emptyState } = props;
  const rowRefs = useRef(new Map<string, HTMLElement>());
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());

  const totalItems = useMemo(() => groups.reduce((n, g) => n + g.items.length, 0), [groups]);

  // Flatten groups → navigable entries (headers only when a label is present;
  // collapsed groups contribute their header only).
  const entries = useMemo<Entry[]>(() => {
    const out: Entry[] = [];
    for (const g of groups) {
      const hasHeader = g.label != null;
      if (hasHeader) {
        out.push({ kind: 'header', key: `h:${g.key}`, groupKey: g.key, label: g.label, hint: g.hint, count: g.items.length });
      }
      if (hasHeader && collapsed.has(g.key)) continue;
      for (const item of g.items) out.push({ kind: 'row', key: `r:${item.id}`, groupKey: g.key, item });
    }
    return out;
  }, [groups, collapsed]);

  const rows = useMemo(() => entries.filter((e): e is Extract<Entry, { kind: 'row' }> => e.kind === 'row'), [entries]);
  const currentRowIdx = useMemo(
    () => (selectedId ? rows.findIndex((r) => r.item.id === selectedId) : -1),
    [rows, selectedId],
  );

  // Scroll the active row into view when it changes.
  useEffect(() => {
    if (!selectedId) return;
    rowRefs.current.get(`r:${selectedId}`)?.scrollIntoView({ block: 'nearest' });
  }, [selectedId]);

  const toggleCollapse = useCallback((groupKey: string, next?: boolean) => {
    setCollapsed((prev) => {
      const willCollapse = next ?? !prev.has(groupKey);
      const s = new Set(prev);
      if (willCollapse) s.add(groupKey);
      else s.delete(groupKey);
      return s;
    });
  }, []);

  const groupKeyOfSelected = useMemo(
    () => rows.find((r) => r.item.id === selectedId)?.groupKey,
    [rows, selectedId],
  );

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (isEditableTarget(e.target)) return;

    // ⌘Z / Ctrl+Z — undo the last archive.
    if ((e.metaKey || e.ctrlKey) && (e.key === 'z' || e.key === 'Z')) {
      e.preventDefault();
      props.onUndo();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;

    const curItem = currentRowIdx >= 0 ? rows[currentRowIdx].item : undefined;

    switch (e.key) {
      case 'j':
      case 'ArrowDown':
        e.preventDefault();
        if (rows.length) props.onSelect(rows[Math.min(currentRowIdx + 1, rows.length - 1)].item);
        break;
      case 'k':
      case 'ArrowUp':
        e.preventDefault();
        if (rows.length) props.onSelect(rows[Math.max(currentRowIdx - 1, 0)].item);
        break;
      case 'ArrowLeft':
        e.preventDefault();
        if (groupKeyOfSelected) toggleCollapse(groupKeyOfSelected, true);
        break;
      case 'ArrowRight':
        e.preventDefault();
        if (groupKeyOfSelected) toggleCollapse(groupKeyOfSelected, false);
        break;
      case 'Enter':
        e.preventDefault();
        if (curItem) props.onSelect(curItem);
        break;
      case 'a':
        if (curItem && canArchive) {
          e.preventDefault();
          props.onArchive(curItem);
        }
        break;
      case 'u':
      case 'U':
        if (curItem) {
          e.preventDefault();
          props.onUnread(curItem);
        }
        break;
    }
  };

  if (totalItems === 0) return <>{emptyState}</>;

  const setRef = (key: string) => (el: HTMLElement | null) => {
    if (el) rowRefs.current.set(key, el);
    else rowRefs.current.delete(key);
  };

  return (
    <div
      role="listbox"
      tabIndex={0}
      aria-label="inbox"
      onKeyDown={onKeyDown}
      className="space-y-0.5 rounded-md outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
    >
      {entries.map((entry) => {
        if (entry.kind === 'header') {
          return (
            <GroupHeaderRow
              key={entry.key}
              label={entry.label}
              hint={entry.hint}
              count={entry.count}
              collapsed={collapsed.has(entry.groupKey)}
              onToggle={() => toggleCollapse(entry.groupKey)}
            />
          );
        }
        return (
          <div key={entry.key} ref={setRef(entry.key)}>
            <InboxRow
              item={entry.item}
              selected={entry.item.id === selectedId}
              unread={isUnread(entry.item)}
              canArchive={canArchive}
              agentName={agentName(entry.item.agentId ?? '')}
              labels={labels}
              onSelect={() => props.onSelect(entry.item)}
              onArchive={() => props.onArchive(entry.item)}
            />
          </div>
        );
      })}
    </div>
  );
}
