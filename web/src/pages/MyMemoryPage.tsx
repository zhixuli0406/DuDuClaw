import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Brain, Info, Loader2, Trash2, TriangleAlert } from 'lucide-react';
import { api, type MyMemoryItem, type MyMemorySourceKind } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAgentAvatar } from '@/stores/agent-avatar-store';
import { ConfirmDialog } from '@/components/settings/controls';
import { Button, Skeleton } from '@/components/mds';
import { formatError, toast } from '@/lib/toast';
import { cn } from '@/lib/utils';

/** RM-039 「我的記憶」 — what AI employees automatically remember about the signed-in user. */

const PAGE_SIZE = 20;

/** The three plain-language source groups the design shows. */
type SourceGroup = 'conversation' | 'explicit' | 'other';

export function sourceGroupOf(kind: MyMemorySourceKind, explicit: boolean): SourceGroup {
  if (explicit || kind === 'explicit') return 'explicit';
  if (kind === 'conversation') return 'conversation';
  return 'other';
}

/** Error codes `me.memory.*` returns as `{ code, message }` (see `my_memory_rpc.rs`). */
const MY_MEMORY_ERROR_CODES = [
  'memory_not_found',
  'memory_login_required',
  'memory_account_unavailable',
  'memory_invalid_cursor',
  'memory_invalid_id',
  'memory_read_failed',
  'memory_forget_failed',
  'memory_forget_record_failed',
  'memory_unsupported',
] as const;
type MyMemoryErrorCode = (typeof MY_MEMORY_ERROR_CODES)[number];

/** The known error code of a `me.memory.*` rejection, if it carries one. */
export function myMemoryErrorCode(err: unknown): MyMemoryErrorCode | null {
  if (!err || typeof err !== 'object' || !('code' in err)) return null;
  const code = (err as { code?: unknown }).code;
  return typeof code === 'string' && (MY_MEMORY_ERROR_CODES as readonly string[]).includes(code)
    ? (code as MyMemoryErrorCode)
    : null;
}

const SOURCE_TAG_CLASS: Record<SourceGroup, string> = {
  conversation: 'bg-muted text-muted-foreground',
  explicit: 'bg-brand/15 text-brand',
  other: 'text-muted-foreground ring-1 ring-inset ring-border',
};

const AVATAR_TONES = [
  'bg-brand/15 text-brand',
  'bg-success/15 text-success',
  'bg-warning/15 text-warning',
  'bg-muted text-muted-foreground',
];

function toneFor(id: string): string {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return AVATAR_TONES[h % AVATAR_TONES.length];
}

function AgentChip({ id, name }: { id: string; name: string }) {
  const src = useAgentAvatar(id);
  const initial = Array.from(name.trim())[0]?.toUpperCase() ?? '?';
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        aria-hidden="true"
        className={cn(
          'inline-flex size-5 shrink-0 items-center justify-center overflow-hidden rounded-full text-[10px] font-semibold ring-1 ring-border',
          toneFor(id),
        )}
      >
        {src ? <img src={src} alt="" className="size-full object-cover" /> : initial}
      </span>
      <span className="min-w-0 truncate">{name}</span>
    </span>
  );
}

const SELECT_CLASS =
  'h-11 min-w-0 flex-1 rounded-lg border border-input bg-transparent px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 md:h-9 md:flex-none md:min-w-40 dark:bg-input/30';

export function MyMemoryPage() {
  const intl = useIntl();
  const t = useCallback(
    (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `myMemory.${key}` }, values),
    [intl],
  );
  const agents = useAgentsStore((s) => s.agents);

  const [items, setItems] = useState<MyMemoryItem[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [failed, setFailed] = useState(false);
  const [sourceFilter, setSourceFilter] = useState<'all' | SourceGroup>('all');
  const [agentFilter, setAgentFilter] = useState('all');
  const [pendingDelete, setPendingDelete] = useState<MyMemoryItem | null>(null);
  const [deleting, setDeleting] = useState(false);
  const epoch = useRef(0);

  /** Localized copy for a coded rejection; generic formatting otherwise. */
  const errorText = (e: unknown) => {
    const code = myMemoryErrorCode(e);
    return code ? t(`errorCode.${code}`) : formatError(e);
  };

  const loadFirst = useCallback(async () => {
    const current = ++epoch.current;
    setLoading(true);
    setFailed(false);
    try {
      const page = await api.myMemory.list({ limit: PAGE_SIZE });
      if (current !== epoch.current) return;
      setItems(page.items);
      setCursor(page.next_cursor);
    } catch {
      if (current !== epoch.current) return;
      setItems([]);
      setCursor(null);
      setFailed(true);
    } finally {
      if (current === epoch.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadFirst();
    return () => { ++epoch.current; };
  }, [loadFirst]);

  const loadMore = async () => {
    if (!cursor || loadingMore) return;
    const current = epoch.current;
    setLoadingMore(true);
    try {
      const page = await api.myMemory.list({ cursor, limit: PAGE_SIZE });
      if (current !== epoch.current) return;
      setItems((old) => {
        const seen = new Set(old.map((i) => i.id));
        return [...old, ...page.items.filter((i) => !seen.has(i.id))];
      });
      setCursor(page.next_cursor);
    } catch (e) {
      if (current === epoch.current) toast.error(errorText(e));
    } finally {
      setLoadingMore(false);
    }
  };

  const agentName = useCallback(
    (id: string) => {
      const found = agents.find((a) => a.name === id);
      return found?.display_name || id;
    },
    [agents],
  );

  const sourceLabel = (group: SourceGroup) => t(`source.${group}`);

  const agentOptions = useMemo(
    () => Array.from(new Set(items.map((i) => i.agent))).map((id) => ({ id, name: agentName(id) })),
    [items, agentName],
  );
  // A selected employee whose rows were all deleted must not strand the filter.
  const agentFilterValid = agentFilter === 'all' || agentOptions.some((o) => o.id === agentFilter);
  const activeAgent = agentFilterValid ? agentFilter : 'all';

  const visible = items.filter(
    (i) =>
      (sourceFilter === 'all' || sourceGroupOf(i.source_kind, i.explicit) === sourceFilter) &&
      (activeAgent === 'all' || i.agent === activeAgent),
  );
  const filtering = sourceFilter !== 'all' || activeAgent !== 'all';

  const formatTime = (iso: string) => {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return '';
    const now = new Date();
    const sameDay = (a: Date, b: Date) => a.toDateString() === b.toDateString();
    const yesterday = new Date(now);
    yesterday.setDate(now.getDate() - 1);
    const hm = d.toLocaleTimeString(intl.locale, { hour: '2-digit', minute: '2-digit', hour12: false });
    if (sameDay(d, now)) return t('time.today', { time: hm });
    if (sameDay(d, yesterday)) return t('time.yesterday', { time: hm });
    const date = d.toLocaleDateString(intl.locale, {
      month: '2-digit',
      day: '2-digit',
      ...(d.getFullYear() !== now.getFullYear() ? { year: 'numeric' as const } : {}),
    });
    return `${date} ${hm}`;
  };

  const confirmDelete = async () => {
    const target = pendingDelete;
    if (!target || deleting) return;
    setDeleting(true);
    try {
      await api.myMemory.forget(target.id);
      setItems((old) => old.filter((i) => i.id !== target.id));
      toast.success(t('deleted'));
      setPendingDelete(null);
    } catch (e) {
      if (myMemoryErrorCode(e) === 'memory_not_found') {
        // Already gone server-side — reflect that instead of leaving a ghost row.
        setItems((old) => old.filter((i) => i.id !== target.id));
        toast.info(t('errorCode.memory_not_found'));
      } else {
        toast.error(errorText(e));
      }
      setPendingDelete(null);
    } finally {
      setDeleting(false);
    }
  };

  const showList = !loading && !failed && items.length > 0;

  return (
    <div className="mx-auto w-full max-w-[880px] space-y-4 p-4 md:p-6">
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h1 className="text-base font-medium">{t('title')}</h1>
        {showList && (
          <span className="font-mono text-xs tabular-nums text-muted-foreground" data-testid="my-memory-count">
            {items.length}
          </span>
        )}
        <span className="text-sm text-muted-foreground">{t('subtitle')}</span>
      </header>

      <p role="note" className="flex items-start gap-2.5 rounded-lg bg-muted px-3.5 py-3 text-sm text-foreground/80">
        <Info aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
        <span>{t('note')}</span>
      </p>

      {showList && (
        <div className="flex gap-2 md:flex-wrap md:items-center md:gap-2.5">
          <select
            className={SELECT_CLASS}
            aria-label={t('filter.source')}
            value={sourceFilter}
            onChange={(e) => setSourceFilter(e.target.value as 'all' | SourceGroup)}
          >
            <option value="all">{t('filter.sourceAll')}</option>
            {(['conversation', 'explicit', 'other'] as const).map((g) => (
              <option key={g} value={g}>{sourceLabel(g)}</option>
            ))}
          </select>
          <select
            className={SELECT_CLASS}
            aria-label={t('filter.agent')}
            value={activeAgent}
            onChange={(e) => setAgentFilter(e.target.value)}
          >
            <option value="all">{t('filter.agentAll')}</option>
            {agentOptions.map((o) => (
              <option key={o.id} value={o.id}>{o.name}</option>
            ))}
          </select>
        </div>
      )}

      {loading && (
        <div
          aria-busy="true"
          aria-label={t('loading')}
          className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-card"
        >
          {[72, 58, 64].map((w) => (
            <div key={w} className="space-y-2 px-5 py-4">
              <Skeleton className="h-3" style={{ width: `${w}%` }} />
              <div className="flex gap-2.5">
                <Skeleton className="h-3 w-20" />
                <Skeleton className="h-3 w-14" />
                <Skeleton className="h-3 w-[70px] rounded-full" />
              </div>
            </div>
          ))}
        </div>
      )}

      {failed && !loading && (
        <div role="alert" className="flex flex-col items-center gap-2.5 rounded-xl border border-border bg-card px-6 py-12 text-center">
          <span className="flex size-11 items-center justify-center rounded-full bg-destructive/10 text-destructive">
            <TriangleAlert aria-hidden="true" className="size-5" />
          </span>
          <p className="text-[15px] font-medium">{t('error.title')}</p>
          <p className="max-w-sm text-sm text-muted-foreground">{t('error.body')}</p>
          <Button variant="outline" size="sm" className="min-h-11 md:min-h-0" onClick={() => void loadFirst()}>
            {t('error.retry')}
          </Button>
        </div>
      )}

      {!loading && !failed && items.length === 0 && (
        <div className="flex flex-col items-center gap-2.5 rounded-xl border border-border bg-card px-6 py-12 text-center">
          <span className="flex size-11 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Brain aria-hidden="true" className="size-5" />
          </span>
          <p className="text-[15px] font-medium">{t('empty.title')}</p>
          <p className="max-w-sm text-sm text-muted-foreground">{t('empty.body')}</p>
        </div>
      )}

      {showList && visible.length === 0 && (
        <p className="rounded-xl border border-border bg-card px-6 py-10 text-center text-sm text-muted-foreground">
          {t(cursor ? 'filter.noneLoadMore' : 'filter.none')}
        </p>
      )}

      {showList && visible.length > 0 && (
        <ul
          aria-label={t('list')}
          className="space-y-3 md:space-y-0 md:divide-y md:divide-border md:overflow-hidden md:rounded-xl md:border md:border-border md:bg-card"
        >
          {visible.map((item) => {
            const group = sourceGroupOf(item.source_kind, item.explicit);
            return (
              <li
                key={item.id}
                className="flex items-start justify-between gap-2 rounded-xl border border-border bg-card py-3.5 pl-3.5 pr-2 md:gap-5 md:rounded-none md:border-0 md:bg-transparent md:px-5 md:py-4"
              >
                <div className="min-w-0 flex-1">
                  <p className="break-words text-sm">{item.text}</p>
                  <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs md:text-[13px]">
                    <AgentChip id={item.agent} name={agentName(item.agent)} />
                    <time dateTime={item.created_at} className="font-mono tabular-nums text-muted-foreground">
                      {formatTime(item.created_at)}
                    </time>
                    <span className={cn('inline-flex h-[22px] items-center rounded-full px-2.5 text-xs font-medium whitespace-nowrap', SOURCE_TAG_CLASS[group])}>
                      {sourceLabel(group)}
                    </span>
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  aria-label={t('delete.aria')}
                  className="size-11 shrink-0 p-0 md:size-auto md:px-2.5"
                  onClick={() => setPendingDelete(item)}
                >
                  <Trash2 aria-hidden="true" />
                  <span className="sr-only md:not-sr-only">{t('delete.action')}</span>
                </Button>
              </li>
            );
          })}
        </ul>
      )}

      {showList && (
        <div className="flex flex-col items-center gap-2">
          {cursor && (
            <Button
              variant="outline"
              className="min-h-11 w-full md:min-h-0 md:w-auto"
              disabled={loadingMore}
              onClick={() => void loadMore()}
            >
              {loadingMore && <Loader2 aria-hidden="true" className="animate-spin" />}
              {loadingMore ? t('loadingMore') : t('loadMore')}
            </Button>
          )}
          <span className="text-xs text-muted-foreground" data-testid="my-memory-shown">
            {filtering
              ? t('shownFiltered', { shown: visible.length, loaded: items.length })
              : t('shown', { count: items.length })}
          </span>
        </div>
      )}

      <ConfirmDialog
        open={pendingDelete !== null}
        onClose={() => { if (!deleting) setPendingDelete(null); }}
        onConfirm={() => void confirmDelete()}
        title={t('confirm.title')}
        message={t('confirm.body')}
        confirmLabel={t('delete.action')}
        busy={deleting}
      >
        {pendingDelete && (
          <blockquote className="rounded-lg bg-muted px-3 py-2.5 text-sm">
            <span className="break-words">{pendingDelete.text}</span>
            <span className="mt-1 block text-xs text-muted-foreground">
              {agentName(pendingDelete.agent)} · {sourceLabel(sourceGroupOf(pendingDelete.source_kind, pendingDelete.explicit))}
            </span>
          </blockquote>
        )}
      </ConfirmDialog>
    </div>
  );
}

export default MyMemoryPage;
