import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import { cn } from '@/lib/utils';
import { GroupHeader } from '@/components/ui';
import type { InboxColumn, InboxItem } from '@/lib/inbox-model';
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
  columns: readonly InboxColumn[];
  canArchive: boolean;
  agentName: (id: string) => string;
  labels: InboxRowLabels;
  emptyState: ReactNode;
  onOpen: (item: InboxItem) => void;
  onApprove: (item: InboxItem) => void;
  onReject: (item: InboxItem) => void;
  onView: (item: InboxItem) => void;
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

/**
 * InboxList — renders precomputed groups and owns the keyboard flow (§5.2 T4.3):
 * `j`/`k` (or ↑/↓) move, `←`/`→` collapse/expand the group, `Enter` opens (or
 * toggles a header), `a` archives (when allowed), `U` marks unread, `⌘Z` undoes.
 * Hover follows the keyboard cursor. Selection is tracked by a stable key so it
 * survives collapse/rebuild.
 */
export function InboxList(props: InboxListProps) {
  const { groups, columns, canArchive, agentName, labels, emptyState } = props;
  const containerRef = useRef<HTMLDivElement>(null);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const [selectedKey, setSelectedKey] = useState<string | null>(null);

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

  const selectedIdx = useMemo(() => {
    const i = entries.findIndex((e) => e.key === selectedKey);
    return i >= 0 ? i : 0;
  }, [entries, selectedKey]);

  // Keep a valid selection as entries change.
  useEffect(() => {
    if (entries.length === 0) return;
    if (!selectedKey || !entries.some((e) => e.key === selectedKey)) {
      const firstRow = entries.find((e) => e.kind === 'row') ?? entries[0];
      setSelectedKey(firstRow.key);
    }
  }, [entries, selectedKey]);

  // Scroll the active entry into view when it changes.
  useEffect(() => {
    if (!selectedKey) return;
    rowRefs.current.get(selectedKey)?.scrollIntoView({ block: 'nearest' });
  }, [selectedKey]);

  const toggleCollapse = useCallback((groupKey: string, next?: boolean) => {
    setCollapsed((prev) => {
      const willCollapse = next ?? !prev.has(groupKey);
      const s = new Set(prev);
      if (willCollapse) s.add(groupKey);
      else s.delete(groupKey);
      return s;
    });
  }, []);

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (isEditableTarget(e.target)) return;
    const cur = entries[selectedIdx];

    // ⌘Z / Ctrl+Z — undo the last archive.
    if ((e.metaKey || e.ctrlKey) && (e.key === 'z' || e.key === 'Z')) {
      e.preventDefault();
      props.onUndo();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;

    switch (e.key) {
      case 'j':
      case 'ArrowDown':
        e.preventDefault();
        if (entries.length) setSelectedKey(entries[Math.min(selectedIdx + 1, entries.length - 1)].key);
        break;
      case 'k':
      case 'ArrowUp':
        e.preventDefault();
        if (entries.length) setSelectedKey(entries[Math.max(selectedIdx - 1, 0)].key);
        break;
      case 'ArrowLeft':
        e.preventDefault();
        if (!cur) break;
        if (cur.kind === 'header') {
          toggleCollapse(cur.groupKey, true);
        } else {
          // Collapse the row's group and land on its header.
          toggleCollapse(cur.groupKey, true);
          setSelectedKey(`h:${cur.groupKey}`);
        }
        break;
      case 'ArrowRight':
        e.preventDefault();
        if (cur?.kind === 'header') toggleCollapse(cur.groupKey, false);
        break;
      case 'Enter':
        if (!cur) break;
        e.preventDefault();
        if (cur.kind === 'header') toggleCollapse(cur.groupKey);
        else props.onOpen(cur.item);
        break;
      case 'a':
        if (cur?.kind === 'row' && canArchive) {
          e.preventDefault();
          props.onArchive(cur.item);
        }
        break;
      case 'u':
      case 'U':
        if (cur?.kind === 'row') {
          e.preventDefault();
          props.onUnread(cur.item);
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
      ref={containerRef}
      role="listbox"
      tabIndex={0}
      aria-label="inbox"
      onKeyDown={onKeyDown}
      className="space-y-2 rounded-card focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/30"
    >
      {entries.map((entry) => {
        const selected = entry.key === selectedKey;
        if (entry.kind === 'header') {
          return (
            <div key={entry.key} ref={setRef(entry.key)} className={cn('px-1 pt-3', selected && 'rounded-control ring-1 ring-amber-500/40')}>
              <GroupHeader
                label={entry.label}
                count={entry.count}
                collapsed={collapsed.has(entry.groupKey)}
                onToggle={() => {
                  setSelectedKey(entry.key);
                  toggleCollapse(entry.groupKey);
                }}
              />
              {entry.hint && !collapsed.has(entry.groupKey) && (
                <p className="px-6 pb-1 text-xs text-stone-400 dark:text-stone-500">{entry.hint}</p>
              )}
            </div>
          );
        }
        return (
          <div key={entry.key} ref={setRef(entry.key)}>
            <InboxRow
              item={entry.item}
              selected={selected}
              columns={columns}
              canArchive={canArchive}
              agentName={agentName(entry.item.agentId ?? '')}
              labels={labels}
              onSelect={() => setSelectedKey(entry.key)}
              onOpen={() => props.onOpen(entry.item)}
              onApprove={() => props.onApprove(entry.item)}
              onReject={() => props.onReject(entry.item)}
              onView={() => props.onView(entry.item)}
              onArchive={() => props.onArchive(entry.item)}
            />
          </div>
        );
      })}
    </div>
  );
}
