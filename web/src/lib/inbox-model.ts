/**
 * Unified "needs me" item model (dashboard-redesign-v2 §5.2). The Inbox merges
 * five gateway sources — approvals / decisions / blocked tasks / budget
 * incidents / failed runs — into ONE clearable queue. Each source maps into
 * this shape; the list renders type-agnostically off it.
 *
 * This module is the pure logic layer (V4-T4.1): tab semantics, sorting,
 * grouping, the blocked tri-bucket classifier, and read/archive persistence.
 * Everything here is a pure function or a thin localStorage wrapper so it is
 * unit-testable in isolation from React.
 */
import type { RiskLevel } from './approval-risk';

export type InboxItemType = 'approval' | 'decision' | 'blocked' | 'budget' | 'failed_run';

export interface InboxItem {
  /** Unique across all types — always prefixed with the type (`approval:<id>`). */
  readonly id: string;
  readonly type: InboxItemType;
  /** One-line human title. */
  readonly title: string;
  /** Originating AI staff id, when the item belongs to one. */
  readonly agentId?: string;
  /** Channel of origin, when applicable (failed runs). Drives group-by channel. */
  readonly channel?: string;
  /** ISO timestamp for `timeAgo` + time/stuck sort. */
  readonly timestamp?: string;
  /** Higher = more urgent; drives the "urgency" sort. */
  readonly urgency: number;
  /** Whether a primary "act" (approve / jump) is available. */
  readonly actionable: boolean;
  /** Free-form status token (task status / incident severity) — the "全部" tab filters on it. */
  readonly status?: string;
  /** Whole-action risk band (approvals only) — drives the row's risk badge (U2). */
  readonly risk?: RiskLevel;
}

/** The five inbox lenses (§5.2). */
export type InboxTab = 'mine' | 'recent' | 'unread' | 'blocked' | 'all';
export const INBOX_TABS: readonly InboxTab[] = ['mine', 'recent', 'unread', 'blocked', 'all'];

export type InboxGroupBy = 'none' | 'type' | 'agent' | 'channel';
export type InboxSortBy = 'urgency' | 'time' | 'stuck';

/** Which optional columns a row shows (leading avatar + title are always on). */
export type InboxColumn = 'type' | 'agent' | 'channel' | 'time';
export const ALL_COLUMNS: readonly InboxColumn[] = ['type', 'agent', 'channel', 'time'];
export const DEFAULT_COLUMNS: readonly InboxColumn[] = ['type', 'agent', 'time'];

/** Blocked-tab tri-classification (§5.2): what does this item need from me? */
export type BlockedBucket = 'decide' | 'input' | 'attention';
export const BLOCKED_BUCKET_ORDER: readonly BlockedBucket[] = ['decide', 'input', 'attention'];

/** Recent window for the "最近" tab. */
export const RECENT_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

/** Baseline urgency per type, so the queue orders sensibly before timestamps. */
export const TYPE_URGENCY: Record<InboxItemType, number> = {
  budget: 40,
  failed_run: 35,
  blocked: 30,
  approval: 20,
  decision: 10,
};

/** Parse an item timestamp to epoch ms, or null when absent/invalid. */
function tsMs(item: InboxItem): number | null {
  if (!item.timestamp) return null;
  const t = Date.parse(item.timestamp);
  return Number.isFinite(t) ? t : null;
}

// ── Sorting ─────────────────────────────────────────────────────────────────

export function sortInbox(items: readonly InboxItem[], by: InboxSortBy): InboxItem[] {
  const copy = [...items];
  if (by === 'time') {
    // Newest first; undated sink to the bottom.
    return copy.sort((a, b) => (tsMs(b) ?? -Infinity) - (tsMs(a) ?? -Infinity));
  }
  if (by === 'stuck') {
    // "卡最久" — oldest first; undated sink to the bottom.
    return copy.sort((a, b) => (tsMs(a) ?? Infinity) - (tsMs(b) ?? Infinity));
  }
  // urgency: type weight, tie-broken by newest.
  return copy.sort((a, b) => b.urgency - a.urgency || (tsMs(b) ?? -Infinity) - (tsMs(a) ?? -Infinity));
}

// ── Tab filtering ────────────────────────────────────────────────────────────

export interface InboxContext {
  /** Ids the user has marked / opened as read. */
  readonly readIds: ReadonlySet<string>;
  /** Injectable clock for deterministic tests. */
  readonly nowMs?: number;
}

/**
 * Filter to a tab's population. Note on "我的": with the current source set every
 * item is something surfaced to the operator, so "mine" = the actionable subset
 * (things that offer a primary act). This is honest given the data — there is no
 * per-user ownership field on approvals/incidents to filter on. Documented, not
 * faked.
 */
export function filterByTab(items: readonly InboxItem[], tab: InboxTab, ctx: InboxContext): InboxItem[] {
  switch (tab) {
    case 'mine':
      return items.filter((i) => i.actionable);
    case 'recent': {
      const now = ctx.nowMs ?? Date.now();
      return items.filter((i) => {
        const t = tsMs(i);
        return t != null && now - t <= RECENT_WINDOW_MS;
      });
    }
    case 'unread':
      return items.filter((i) => !ctx.readIds.has(i.id));
    case 'blocked':
      // Everything that maps into a "what does it need from me" bucket. In the
      // current source set that is all types; the value of this tab is the
      // tri-bucket re-organisation (blockedBucket) + stuck-sort, not a narrower
      // population.
      return [...items];
    case 'all':
      return [...items];
  }
}

/** Classify an item into the blocked-tab tri-bucket. */
export function blockedBucket(item: InboxItem): BlockedBucket {
  if (item.type === 'approval' || item.type === 'decision') return 'decide';
  if (item.type === 'blocked') return item.agentId ? 'input' : 'attention';
  // budget / failed_run — nothing to decide, just needs eyes.
  return 'attention';
}

// ── Grouping ─────────────────────────────────────────────────────────────────

/** Group bucket key for an item under a group-by mode. `''` = single bucket. */
export function groupKeyOf(item: InboxItem, by: InboxGroupBy): string {
  switch (by) {
    case 'none':
      return '';
    case 'type':
      return item.type;
    case 'agent':
      return item.agentId ?? '—';
    case 'channel':
      return item.channel ?? '—';
  }
}

// ── "全部" tab filters ─────────────────────────────────────────────────────────

export function filterByCategory(items: readonly InboxItem[], category: InboxItemType | 'all'): InboxItem[] {
  if (category === 'all') return [...items];
  return items.filter((i) => i.type === category);
}

export function filterByStatus(items: readonly InboxItem[], status: string | 'all'): InboxItem[] {
  if (status === 'all') return [...items];
  return items.filter((i) => (i.status ?? '') === status);
}

/** Distinct non-empty status tokens present, sorted, for the status Select. */
export function distinctStatuses(items: readonly InboxItem[]): string[] {
  const set = new Set<string>();
  for (const i of items) if (i.status) set.add(i.status);
  return [...set].sort();
}

/** Exclude archived ids (archived rows never render). */
export function excludeArchived(items: readonly InboxItem[], archived: ReadonlySet<string>): InboxItem[] {
  if (archived.size === 0) return [...items];
  return items.filter((i) => !archived.has(i.id));
}

// ── Immutable id-set helpers (read / archived membership) ──────────────────────

export function withId(set: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(set);
  next.add(id);
  return next;
}

export function withoutId(set: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(set);
  next.delete(id);
  return next;
}

// ── localStorage persistence ──────────────────────────────────────────────────

export const READ_KEY = 'duduclaw:inbox:read';
export const ARCHIVED_KEY = 'duduclaw:inbox:archived';
export const PREFS_KEY = 'duduclaw:inbox:prefs';

/** Cap on how many ids we retain per set, newest-wins, to bound storage growth. */
const ID_SET_CAP = 500;

export function loadIdSet(key: string): Set<string> {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? new Set(parsed.filter((x): x is string => typeof x === 'string')) : new Set();
  } catch {
    return new Set();
  }
}

export function persistIdSet(key: string, set: ReadonlySet<string>): void {
  try {
    // Keep the tail (most-recently added survive the cap).
    const arr = [...set].slice(-ID_SET_CAP);
    localStorage.setItem(key, JSON.stringify(arr));
  } catch {
    /* private mode — preference just won't persist */
  }
}

export interface InboxPrefs {
  tab: InboxTab;
  groupBy: InboxGroupBy;
  sortBy: InboxSortBy;
  columns: InboxColumn[];
  categoryFilter: InboxItemType | 'all';
  statusFilter: string | 'all';
}

export const DEFAULT_PREFS: InboxPrefs = {
  tab: 'mine',
  groupBy: 'type',
  sortBy: 'urgency',
  columns: [...DEFAULT_COLUMNS],
  categoryFilter: 'all',
  statusFilter: 'all',
};

const TAB_SET = new Set<InboxTab>(INBOX_TABS);
const GROUP_SET = new Set<InboxGroupBy>(['none', 'type', 'agent', 'channel']);
const SORT_SET = new Set<InboxSortBy>(['urgency', 'time', 'stuck']);
const COL_SET = new Set<InboxColumn>(ALL_COLUMNS);

/** Load prefs, defaulting any missing/invalid field (never throws). */
export function loadPrefs(): InboxPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_PREFS };
    const p = JSON.parse(raw) as Partial<InboxPrefs>;
    const columns = Array.isArray(p.columns) ? p.columns.filter((c): c is InboxColumn => COL_SET.has(c as InboxColumn)) : [];
    return {
      tab: TAB_SET.has(p.tab as InboxTab) ? (p.tab as InboxTab) : DEFAULT_PREFS.tab,
      groupBy: GROUP_SET.has(p.groupBy as InboxGroupBy) ? (p.groupBy as InboxGroupBy) : DEFAULT_PREFS.groupBy,
      sortBy: SORT_SET.has(p.sortBy as InboxSortBy) ? (p.sortBy as InboxSortBy) : DEFAULT_PREFS.sortBy,
      columns: columns.length ? columns : [...DEFAULT_COLUMNS],
      categoryFilter: typeof p.categoryFilter === 'string' ? p.categoryFilter : 'all',
      statusFilter: typeof p.statusFilter === 'string' ? p.statusFilter : 'all',
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function persistPrefs(prefs: InboxPrefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* private mode */
  }
}
