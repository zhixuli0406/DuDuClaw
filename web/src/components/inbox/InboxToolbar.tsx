import { useIntl } from 'react-intl';
import { Columns3, Undo2, CheckCheck } from 'lucide-react';
import { Button } from '@/components/mds';
import { cn } from '@/lib/utils';

const controlClass =
  'h-8 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 text-sm text-foreground placeholder:text-muted-foreground outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30';
import {
  ALL_COLUMNS,
  type InboxColumn,
  type InboxGroupBy,
  type InboxItemType,
  type InboxSortBy,
} from '@/lib/inbox-model';

const GROUP_OPTIONS: InboxGroupBy[] = ['none', 'type', 'agent', 'channel'];
const SORT_OPTIONS: InboxSortBy[] = ['urgency', 'time', 'stuck'];
const CATEGORY_OPTIONS: (InboxItemType | 'all')[] = ['all', 'approval', 'decision', 'blocked', 'budget', 'failed_run'];

export interface InboxToolbarProps {
  showAllFilters: boolean;
  /** The Blocked tab forces its own three-bucket grouping, so the group-by
   *  control is a no-op there and is hidden rather than shown dead (A4). */
  showGroupBy: boolean;
  groupBy: InboxGroupBy;
  onGroupBy: (v: InboxGroupBy) => void;
  sortBy: InboxSortBy;
  onSortBy: (v: InboxSortBy) => void;
  columns: readonly InboxColumn[];
  onToggleColumn: (c: InboxColumn) => void;
  categoryFilter: InboxItemType | 'all';
  onCategory: (v: InboxItemType | 'all') => void;
  statuses: readonly string[];
  statusFilter: string;
  onStatus: (v: string) => void;
  hasUndo: boolean;
  onUndo: () => void;
  onMarkAllRead: () => void;
}

export function InboxToolbar(props: InboxToolbarProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const select = cn(controlClass, 'h-8 w-auto pr-8 text-xs');

  return (
    <div className="flex flex-wrap items-center gap-2">
      {/* group-by (hidden on the Blocked tab, which forces bucket grouping — A4) */}
      {props.showGroupBy && (
        <select
          aria-label={t('inbox.group.label')}
          value={props.groupBy}
          onChange={(e) => props.onGroupBy(e.target.value as InboxGroupBy)}
          className={select}
        >
          {GROUP_OPTIONS.map((g) => (
            <option key={g} value={g}>
              {t('inbox.group.label')}: {t(`inbox.group.${g}`)}
            </option>
          ))}
        </select>
      )}

      {/* sort */}
      <select
        aria-label={t('inbox.sort.label')}
        value={props.sortBy}
        onChange={(e) => props.onSortBy(e.target.value as InboxSortBy)}
        className={select}
      >
        {SORT_OPTIONS.map((s) => (
          <option key={s} value={s}>
            {t('inbox.sort.label')}: {t(`inbox.sort.${s}`)}
          </option>
        ))}
      </select>

      {/* column selector (native disclosure) */}
      <details className="relative">
        <summary className="flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-surface-border bg-surface px-3 text-xs text-foreground marker:content-none hover:bg-muted">
          <Columns3 className="h-3.5 w-3.5" />
          {t('inbox.columns.label')}
        </summary>
        <div className="absolute z-20 mt-1 w-40 rounded-xl border border-surface-border bg-surface p-1.5 shadow-[var(--menu-shadow)]">
          {ALL_COLUMNS.map((c) => (
            <label
              key={c}
              className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-xs text-foreground hover:bg-muted"
            >
              <input
                type="checkbox"
                checked={props.columns.includes(c)}
                onChange={() => props.onToggleColumn(c)}
                className="accent-brand"
              />
              {t(`inbox.col.${c}`)}
            </label>
          ))}
        </div>
      </details>

      {props.showAllFilters && (
        <>
          <select
            aria-label={t('inbox.filter.category')}
            value={props.categoryFilter}
            onChange={(e) => props.onCategory(e.target.value as InboxItemType | 'all')}
            className={select}
          >
            {CATEGORY_OPTIONS.map((c) => (
              <option key={c} value={c}>
                {t('inbox.filter.category')}: {c === 'all' ? t('inbox.filter.all') : t(`inbox.type.${c}`)}
              </option>
            ))}
          </select>
          <select
            aria-label={t('inbox.filter.status')}
            value={props.statusFilter}
            onChange={(e) => props.onStatus(e.target.value)}
            className={select}
          >
            <option value="all">
              {t('inbox.filter.status')}: {t('inbox.filter.all')}
            </option>
            {props.statuses.map((s) => (
              <option key={s} value={s}>
                {t('inbox.filter.status')}: {s}
              </option>
            ))}
          </select>
        </>
      )}

      <div className="ml-auto flex items-center gap-2">
        {props.hasUndo && (
          <Button size="sm" variant="ghost" onClick={props.onUndo}>
            <Undo2 />
            {t('inbox.undo')}
          </Button>
        )}
        <Button size="sm" variant="outline" onClick={props.onMarkAllRead}>
          <CheckCheck />
          {t('inbox.markAllRead')}
        </Button>
      </div>
    </div>
  );
}
