import { useState, useCallback, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type MemoryEntry,
  type MemoryChainEntry,
  type MemoryAtRecord,
} from '@/lib/api';
import { client } from '@/lib/ws-client';
import { debounceTrailing } from '@/lib/debounce';
import { useConnectionStore } from '@/stores/connection-store';
import {
  parsePredictionMemory,
  summarizeMemory,
  memoryLayerMessageId,
  memorySourceMessageId,
  toPercent,
  type PredictionMemory,
} from '@/lib/memory-format';
import {
  groupByCategory,
  type MemoryCategoryId,
  type CategoryBucket,
} from '@/lib/memory-category';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import {
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Skeleton,
} from '@/components/mds';
import {
  BrainIcon,
  ClockIcon,
  ArrowRightIcon,
  ActivityIcon,
  HistoryIcon,
  ChevronDownIcon,
  ChevronUpIcon,
  Trash2Icon,
  BriefcaseIcon,
  UsersIcon,
  HandshakeIcon,
  HeartIcon,
  ScrollTextIcon,
  WrenchIcon,
  CalendarClockIcon,
  WalletIcon,
  SparklesIcon,
  BoxesIcon,
  LayersIcon,
} from 'lucide-react';

/**
 * MemoryBrowser — the "記憶" surface, rebuilt for WP5a / decision D6
 * (2026-08-04 client meeting: "make it like Perplexity's memory page").
 *
 * Shape: one memory per row, newest first, each row a single-sentence summary;
 * clicking a row expands the full text plus what the system knows about that
 * memory — which kind it is, how it was recorded, when, its subject/predicate
 * and the supersession chain behind it. The category rail on the left stays as
 * a filter over the same flat list, so the "one row per memory" reading never
 * changes shape between views.
 *
 * Deliberately **no editing** (D6): a memory is either kept or forgotten.
 * Deleting is a soft delete on the backend (`memory.forget` archives the row),
 * so a mis-click is recoverable by an operator, but the UI still asks for a
 * second click before firing.
 *
 * Grouping is deterministic and local (see `lib/memory-category.ts`) — the
 * backend has no topic field and adding an LLM pass per entry would cost on
 * every page view.
 *
 * WP15 (2026-08-04 client feedback: a memory page that was wall-to-wall
 * "學習訊號 70% → 52%" rows, prompting "記憶還沒更新對嗎?"): the main list
 * carries only memories *about the user*. The platform's own learning
 * telemetry — one prediction-deviation row per Moderate-error turn — is a
 * separate, collapsed section at the bottom. The split is made by the gateway
 * (`duduclaw_memory::is_system_signal`, keyed on `source_event`), which
 * returns the two lists under `entries` and `signals`; doing it there rather
 * than filtering here is what stops a telemetry burst from consuming the row
 * limit and evicting real memories entirely.
 */

/** Icon name (from the category table) → the actual lucide component. */
const CATEGORY_ICONS: Record<string, typeof BrainIcon> = {
  Briefcase: BriefcaseIcon,
  Users: UsersIcon,
  Handshake: HandshakeIcon,
  Heart: HeartIcon,
  ScrollText: ScrollTextIcon,
  Wrench: WrenchIcon,
  CalendarClock: CalendarClockIcon,
  Wallet: WalletIcon,
  Activity: ActivityIcon,
  Sparkles: SparklesIcon,
  Boxes: BoxesIcon,
};

/** Newest first — the default order the memory list is read in (WP5a item 2). */
function byRecency(a: MemoryEntry, b: MemoryEntry): number {
  const ta = Date.parse(a.timestamp);
  const tb = Date.parse(b.timestamp);
  // Unparseable timestamps sink to the bottom rather than scrambling the list.
  if (Number.isNaN(ta) && Number.isNaN(tb)) return 0;
  if (Number.isNaN(ta)) return 1;
  if (Number.isNaN(tb)) return -1;
  return tb - ta;
}

export function MemoryBrowser({ agentId, query }: { agentId: string; query: string }) {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [entries, setEntries] = useState<ReadonlyArray<MemoryEntry>>([]);
  /** WP15 — platform learning telemetry, kept out of the list above. */
  const [signals, setSignals] = useState<ReadonlyArray<MemoryEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<MemoryCategoryId | 'all'>('all');

  /** Refetch this agent's memories. `showSpinner` is false for background
   *  refreshes (WP6 live push) so an incoming memory doesn't blank the list
   *  the user is reading. */
  const browse = useCallback(
    async (showSpinner: boolean) => {
      if (!agentId) return;
      if (showSpinner) setLoading(true);
      try {
        const res = await api.memory.browse(agentId, 200);
        setEntries([...(res?.entries ?? [])].sort(byRecency));
        setSignals([...(res?.signals ?? [])].sort(byRecency));
      } catch (e) {
        console.warn('[api]', e);
        toast.error(
          intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }),
        );
        setEntries([]);
        setSignals([]);
      } finally {
        if (showSpinner) setLoading(false);
      }
    },
    [agentId, intl],
  );

  // Browse on agent change. WP5b / D7 — cognitive memory is always-on now, so
  // there is no per-agent flag to look up and no "go enable it" empty state.
  // M3: also keyed on `connectionState`, so a reconnect re-reads once and the
  // view cannot be stranded on state from before the socket dropped.
  useEffect(() => {
    if (!agentId) return;
    if (connectionState !== 'authenticated') return;
    setSelected('all');
    void browse(true);
  }, [agentId, browse, connectionState]);

  // WP6 — data the agent collected or distilled during a channel conversation
  // used to sit in `memory.db` unseen until reload ("看不到的東西使用者會認為
  // 它壞掉"). The gateway pushes `memory.changed` on every write path (MCP
  // `memory_store`, the prediction-driven episodic write on the reply path);
  // refetch when it concerns the agent on screen. Events carrying no
  // `agent_id` are treated as "might be this agent" — refetch rather than
  // risk missing the update. Skipped while a search is active so the push
  // cannot yank the user's result list out from under them.
  // M3: gated on `connectionState`, so a dropped socket re-subscribes on
  // reconnect. The catch-up read itself comes from the browse effect above,
  // which shares the dependency (same shape as RoutinesPage) — events raised
  // while the socket was down are gone, so without that re-read the page would
  // keep showing pre-outage state and look broken.
  // M4: a distill pass raises one event per persisted fact; debounce the burst
  // so one pasted document costs one refetch, not a dozen.
  useEffect(() => {
    if (!agentId) return;
    if (connectionState !== 'authenticated') return;
    const refresh = debounceTrailing(() => void browse(false));
    const unsubscribe = client.subscribe('memory.changed', (payload) => {
      const p = (payload ?? {}) as { agent_id?: string | null };
      if (p.agent_id && p.agent_id !== agentId) return;
      if (query.trim()) return;
      refresh();
    });
    return () => {
      refresh.cancel();
      unsubscribe();
    };
  }, [agentId, query, browse, connectionState]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !agentId) return;
    setLoading(true);
    try {
      const result = await api.memory.search(agentId, query, 200);
      setEntries([...(result?.entries ?? [])].sort(byRecency));
      setSignals([...(result?.signals ?? [])].sort(byRecency));
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
      setSignals([]);
    } finally {
      setLoading(false);
    }
  }, [query, agentId, intl]);

  // Debounced search when the query changes (the search field lives on the
  // page's control row, so we react to `query` here).
  useEffect(() => {
    if (!query.trim()) return;
    const t = setTimeout(() => void handleSearch(), 350);
    return () => clearTimeout(t);
  }, [query, handleSearch]);

  const buckets = useMemo(() => groupByCategory(entries as MemoryEntry[]), [entries]);

  /** Drop a forgotten entry from local state — no refetch, no scroll jump.
   *  Both lists are swept: the same row control appears in either section. */
  const handleForgotten = useCallback((id: string) => {
    setEntries((prev) => prev.filter((e) => e.id !== id));
    setSignals((prev) => prev.filter((e) => e.id !== id));
  }, []);

  if (loading) return <MemoryListSkeleton />;

  // The telemetry section renders under every branch below — including the
  // empty one. That is the honest answer to "記憶還沒更新對嗎?": no memories
  // yet, and here is the machine bookkeeping that is not one.
  const signalSection = <SystemSignals signals={signals} onForgotten={handleForgotten} />;

  if (entries.length === 0) {
    return (
      <div className="flex flex-col gap-6">
        {query.trim() ? (
          <CollectionPageState
            state="empty"
            icon={BrainIcon}
            title={intl.formatMessage({ id: 'memory.empty.search' }, { query })}
          />
        ) : (
          // One empty state, written for someone who has never heard of a
          // "memory layer": say what puts things on this list and what does not.
          <CollectionPageState
            state="empty"
            icon={BrainIcon}
            title={intl.formatMessage({ id: 'memory.empty.memories' })}
            description={intl.formatMessage({ id: 'memory.empty.memories.how' })}
          />
        )}
        {signalSection}
      </div>
    );
  }

  const activeBucket = selected === 'all' ? null : buckets.find((b) => b.category.id === selected);
  // `selected === 'all'` shows every memory in one recency-ordered list; the
  // rail narrows that same list rather than switching to a different layout.
  const visible = selected === 'all' ? entries : (activeBucket?.entries ?? []);

  return (
    <div className="flex flex-col gap-4 md:flex-row md:items-start md:gap-6">
      <CategoryRail
        buckets={buckets}
        total={entries.length}
        selected={selected}
        onSelect={setSelected}
      />
      <div className="min-w-0 flex-1 space-y-6">
        {visible.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={BrainIcon}
            title={intl.formatMessage({ id: 'memory.category.empty' })}
          />
        ) : (
          <div className="flex flex-col gap-1">
            <h2 className="flex items-center gap-2 px-2 pb-1 text-sm font-medium text-foreground">
              {activeBucket ? (
                <>
                  <CategoryIcon name={activeBucket.category.icon} className="size-4 text-brand" />
                  {intl.formatMessage({ id: activeBucket.category.label })}
                </>
              ) : (
                <>
                  <LayersIcon className="size-4 text-brand" />
                  {intl.formatMessage({ id: 'memory.list.recent' })}
                </>
              )}
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {visible.length}
              </span>
            </h2>
            {visible.map((entry) => (
              <MemoryRow key={entry.id} entry={entry} onForgotten={handleForgotten} />
            ))}
          </div>
        )}
        {signalSection}
      </div>
    </div>
  );
}

/**
 * WP15 — the platform's own learning telemetry, parked below the memories in a
 * section that is collapsed until asked for.
 *
 * These rows are not things the user told the agent: the prediction router
 * writes one per turn whose outcome missed its forecast, and the dashboard used
 * to interleave them with real memories, which read as "the memory never
 * updated". Kept visible rather than hidden outright — the count answers "is it
 * doing anything?" without the rows crowding out what the user came to read.
 */
function SystemSignals({
  signals,
  onForgotten,
}: {
  signals: ReadonlyArray<MemoryEntry>;
  onForgotten: (id: string) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  if (signals.length === 0) return null;

  return (
    <section className="rounded-xl border border-surface-border bg-surface-muted/40">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 px-3 py-2.5 text-left"
      >
        {open ? (
          <ChevronUpIcon className="size-4 shrink-0 text-brand" aria-hidden />
        ) : (
          <ChevronDownIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        )}
        <ActivityIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <span className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'memory.signals.title' })}
        </span>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {signals.length}
        </span>
      </button>
      <div className="px-3 pb-3">
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'memory.signals.note' })}
        </p>
        {open && (
          <div className="mt-2 flex flex-col gap-1">
            {signals.map((entry) => (
              <MemoryRow key={entry.id} entry={entry} onForgotten={onForgotten} />
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

/** Lucide icon for a category, resolved by name (the table stays UI-free). */
function CategoryIcon({ name, className }: { name: string; className?: string }) {
  const Icon = CATEGORY_ICONS[name] ?? BoxesIcon;
  return <Icon className={className} />;
}

/**
 * Left rail listing every non-empty category with its count. Horizontal and
 * scrollable on narrow screens, a sticky column from `md` up.
 */
function CategoryRail({
  buckets,
  total,
  selected,
  onSelect,
}: {
  buckets: ReadonlyArray<CategoryBucket<MemoryEntry>>;
  total: number;
  selected: MemoryCategoryId | 'all';
  onSelect: (id: MemoryCategoryId | 'all') => void;
}) {
  const intl = useIntl();
  return (
    <nav
      aria-label={intl.formatMessage({ id: 'memory.category.rail' })}
      className="flex shrink-0 gap-1 overflow-x-auto pb-1 md:sticky md:top-4 md:w-52 md:flex-col md:overflow-visible md:pb-0"
    >
      <RailItem
        icon={<LayersIcon className="size-4" />}
        label={intl.formatMessage({ id: 'memory.category.all' })}
        count={total}
        active={selected === 'all'}
        onClick={() => onSelect('all')}
      />
      {buckets.map((bucket) => (
        <RailItem
          key={bucket.category.id}
          icon={<CategoryIcon name={bucket.category.icon} className="size-4" />}
          label={intl.formatMessage({ id: bucket.category.label })}
          count={bucket.entries.length}
          active={selected === bucket.category.id}
          onClick={() => onSelect(bucket.category.id)}
        />
      ))}
    </nav>
  );
}

function RailItem({
  icon,
  label,
  count,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-current={active ? 'true' : undefined}
      className={cn(
        'flex shrink-0 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-sm transition-colors',
        active
          ? 'bg-accent font-medium text-foreground'
          : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground',
      )}
    >
      <span className={cn('shrink-0', active ? 'text-brand' : 'text-muted-foreground')}>{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">{count}</span>
    </button>
  );
}

function MemoryListSkeleton() {
  return (
    <div className="flex flex-col gap-1.5" role="status" aria-label="Loading">
      {Array.from({ length: 6 }).map((_, i) => (
        <Skeleton key={i} className="h-9 w-full" />
      ))}
    </div>
  );
}

/**
 * Delete affordance: hidden until the row is hovered or focused, and it takes
 * two clicks — the first swaps in an inline confirm so a stray click on a
 * dense list can't silently drop a memory. Keyboard users get the same button
 * via focus-within, so this is not a hover-only control.
 */
function ForgetButton({
  entry,
  onForgotten,
}: {
  entry: MemoryEntry;
  onForgotten: (id: string) => void;
}) {
  const intl = useIntl();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const run = async () => {
    setBusy(true);
    try {
      await api.memory.forget(entry.agent_id, entry.id);
      toast.success(intl.formatMessage({ id: 'memory.forget.done' }));
      onForgotten(entry.id);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'memory.forget.failed' }, { message: formatError(e) }));
      setConfirming(false);
    } finally {
      setBusy(false);
    }
  };

  if (confirming) {
    return (
      <span className="flex shrink-0 items-center gap-1">
        <Button variant="destructive" size="xs" disabled={busy} onClick={() => void run()}>
          {intl.formatMessage({ id: 'memory.forget.confirm' })}
        </Button>
        <Button variant="ghost" size="xs" disabled={busy} onClick={() => setConfirming(false)}>
          {intl.formatMessage({ id: 'common.cancel' })}
        </Button>
      </span>
    );
  }

  return (
    <Button
      variant="ghost"
      size="icon-xs"
      aria-label={intl.formatMessage({ id: 'memory.forget.action' })}
      title={intl.formatMessage({ id: 'memory.forget.action' })}
      onClick={() => setConfirming(true)}
      className="shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 hover:text-destructive"
    >
      <Trash2Icon />
    </Button>
  );
}

/**
 * One memory = one row: a single-sentence summary, when it was recorded, a
 * delete affordance, and a disclosure that opens the full detail panel. The
 * whole row is the toggle (the chevron is decorative and marked
 * `aria-hidden`), so there is one control per row for keyboard users instead
 * of a summary and a separate button competing for the same job.
 */
function MemoryRow({
  entry,
  onForgotten,
}: {
  entry: MemoryEntry;
  onForgotten: (id: string) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const prediction = parsePredictionMemory(entry.content);
  // Prediction telemetry is a fixed English string; never show it raw.
  // Appending the first topic (when the entry carries one) turns "learning
  // signal 70% → 52%" into something that hints at *what* was learned.
  const summary = prediction
    ? `${intl.formatMessage({ id: 'memory.prediction.label' })} · ${toPercent(
        prediction.expected,
      )}% → ${toPercent(prediction.inferred)}%${prediction.topics[0] ? ` · ${prediction.topics[0]}` : ''}`
    : summarizeMemory(entry.content);
  const layerId = memoryLayerMessageId(entry.layer);

  return (
    <div
      className={cn(
        'group rounded-lg border transition-colors',
        open
          ? 'border-surface-border bg-accent/20'
          : 'border-transparent hover:border-surface-border hover:bg-accent/30',
      )}
    >
      <div className="flex min-h-9 items-center gap-2 px-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-start gap-2 py-1.5 text-left"
        >
          {open ? (
            <ChevronUpIcon className="mt-0.5 size-4 shrink-0 text-brand" aria-hidden />
          ) : (
            <ChevronDownIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
          )}
          {/* WP15: two lines, not one. `truncate` cut the summary at whatever
              the column width happened to be, so on a narrow viewport a row
              could show a fraction of its 90-character budget and lose the
              part that identified the memory. */}
          <span className="line-clamp-2 min-w-0 flex-1 text-sm text-foreground">{summary}</span>
        </button>
        {layerId && (
          <Badge variant="secondary" className="hidden shrink-0 sm:inline-flex">
            {intl.formatMessage({ id: layerId })}
          </Badge>
        )}
        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
          {timeAgo(entry.timestamp)}
        </span>
        <ForgetButton entry={entry} onForgotten={onForgotten} />
      </div>
      {open && (
        <div className="space-y-2 px-2 pb-2">
          <MemoryDetail entry={entry} prediction={prediction} />
          <MemoryHistory agentId={entry.agent_id} memoryId={entry.id} />
        </div>
      )}
    </div>
  );
}

/**
 * The expanded half of a row: the memory in full, plus what the system knows
 * about it — which kind of memory it is, how it came to be recorded, exactly
 * when, and how often it has been recalled since. Everything is phrased for an
 * end user; the internal `episodic` / `source_event` vocabulary is translated
 * in `lib/memory-format.ts`.
 */
function MemoryDetail({
  entry,
  prediction,
}: {
  entry: MemoryEntry;
  prediction: PredictionMemory | null;
}) {
  const intl = useIntl();
  const layerId = memoryLayerMessageId(entry.layer);
  const sourceId = memorySourceMessageId(entry.source_event, entry.tags);
  const recorded = Number.isNaN(Date.parse(entry.timestamp))
    ? entry.timestamp
    : new Date(entry.timestamp).toLocaleString();

  return (
    <Card data-size="sm">
      <CardContent className="space-y-3">
        {prediction ? (
          <PredictionDetail data={prediction} />
        ) : (
          <p className="whitespace-pre-wrap text-sm text-foreground">{entry.content}</p>
        )}

        <dl className="grid gap-x-4 gap-y-1.5 border-t border-surface-border pt-3 text-xs sm:grid-cols-2">
          {layerId && (
            <DetailField
              label={intl.formatMessage({ id: 'memory.detail.kind' })}
              value={intl.formatMessage({ id: layerId })}
            />
          )}
          <DetailField
            label={intl.formatMessage({ id: 'memory.detail.source' })}
            value={intl.formatMessage({ id: sourceId })}
          />
          <DetailField
            label={intl.formatMessage({ id: 'memory.detail.recorded' })}
            value={recorded}
          />
          {typeof entry.access_count === 'number' && (
            <DetailField
              label={intl.formatMessage({ id: 'memory.detail.recalled' })}
              value={intl.formatMessage(
                { id: 'memory.detail.recalled.value' },
                { count: entry.access_count },
              )}
            />
          )}
        </dl>

        {entry.tags.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {entry.tags.map((tag) => (
              <Badge key={tag} variant="secondary">
                {tag}
              </Badge>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function DetailField({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words text-foreground">{value}</dd>
    </div>
  );
}

/**
 * A prediction-deviation memory, rendered as a readable "learning signal"
 * instead of the raw English telemetry string. See {@link parsePredictionMemory}.
 */
function PredictionDetail({ data }: { data: PredictionMemory }) {
  const intl = useIntl();
  const expectedPct = toPercent(data.expected);
  const actualPct = toPercent(data.inferred);
  const lower = data.inferred < data.expected;

  return (
    <div className="space-y-2.5">
      <span className="flex items-center gap-1.5 text-xs font-medium text-brand">
        <ActivityIcon className="size-3.5 shrink-0" />
        {intl.formatMessage({ id: 'memory.prediction.label' })}
      </span>
      <p className="text-sm text-foreground">
        {intl.formatMessage({ id: 'memory.prediction.satisfaction' })}{' '}
        <span className="tabular-nums text-muted-foreground">{expectedPct}%</span>
        <ArrowRightIcon className="mx-1 inline size-3 text-muted-foreground" />
        <span className={cn('font-medium tabular-nums', lower ? 'text-destructive' : 'text-success')}>
          {actualPct}%
        </span>
      </p>
      {data.topics.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          <Badge variant="secondary">
            {intl.formatMessage({ id: 'memory.prediction.topics' }, { topics: data.topics.join(', ') })}
          </Badge>
        </div>
      )}
      <div className="flex flex-wrap gap-1.5">
        <Badge variant="secondary">
          {intl.formatMessage({ id: 'memory.prediction.surprise' }, { value: toPercent(data.surprise) })}
        </Badge>
        {data.corrected && (
          <Badge variant="secondary" className="bg-warning/15 text-warning">
            {intl.formatMessage({ id: 'memory.prediction.corrected' })}
          </Badge>
        )}
        {data.followUp && (
          <Badge variant="secondary" className="bg-info/15 text-info">
            {intl.formatMessage({ id: 'memory.prediction.followUp' })}
          </Badge>
        )}
      </div>
      <p className="text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'memory.prediction.note' })}
      </p>
    </div>
  );
}

/**
 * Temporal history / supersession chain for a single memory entry (F1). Lazy:
 * fetches `memory.history` only when the operator expands it. Renders the fact's
 * versions as a timeline (when each became valid, when it was superseded, which
 * one is current) and — when the backend reports a subject/predicate — an
 * optional point-in-time lookup (which value was valid at a chosen moment).
 */
function MemoryHistory({ agentId, memoryId }: { agentId: string; memoryId: string }) {
  const intl = useIntl();
  const [loaded, setLoaded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const [chain, setChain] = useState<ReadonlyArray<MemoryChainEntry>>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [subject, setSubject] = useState('');
  const [predicate, setPredicate] = useState('');

  // Point-in-time query state
  const [atInput, setAtInput] = useState('');
  const [atLoading, setAtLoading] = useState(false);
  const [atResult, setAtResult] = useState<{ found: boolean; record?: MemoryAtRecord } | null>(null);

  useEffect(() => {
    if (loaded || loading) return;
    setLoading(true);
    setFailed(false);
    api.memory.history(agentId, { memory_id: memoryId }).then((res) => {
      setChain(res?.chain ?? []);
      setCurrentId(res?.current_id ?? null);
      setSubject(res?.subject ?? '');
      setPredicate(res?.predicate ?? '');
      setLoaded(true);
    }).catch((e) => {
      console.warn('[api]', e);
      setFailed(true);
    }).finally(() => setLoading(false));
  }, [agentId, memoryId, loaded, loading]);

  const handleAtQuery = async () => {
    if (!atInput || !subject || !predicate) return;
    const parsed = new Date(atInput);
    if (Number.isNaN(parsed.getTime())) return;
    setAtLoading(true);
    setAtResult(null);
    try {
      const res = await api.memory.at(agentId, subject, predicate, parsed.toISOString());
      setAtResult({ found: res?.found ?? false, record: res?.record });
    } catch (e) {
      console.warn('[api]', e);
      setAtResult({ found: false });
    } finally {
      setAtLoading(false);
    }
  };

  return (
    <Card data-size="sm">
      <CardContent>
        <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          <HistoryIcon className="size-3.5" />
          {intl.formatMessage({ id: 'memory.history.toggle' })}
        </div>
        {loading ? (
          <p className="py-2 text-xs text-muted-foreground">{intl.formatMessage({ id: 'common.loading' })}</p>
        ) : failed ? (
          <p className="py-2 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'memory.history.loadError' })}
          </p>
        ) : chain.length === 0 ? (
          <p className="py-2 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'memory.history.empty' })}
          </p>
        ) : (
          <>
            {(subject || predicate) && (
              <p className="mb-3 font-mono text-xs text-muted-foreground">
                {subject} · {predicate}
              </p>
            )}
            <ol className="space-y-0">
              {chain.map((c, i) => {
                const isCurrent = c.is_current || c.id === currentId;
                return (
                  <li key={c.id} className="relative flex gap-3 pb-3 last:pb-0">
                    <div className="relative flex flex-col items-center">
                      <span
                        className={cn(
                          'mt-1 size-2.5 shrink-0 rounded-full',
                          isCurrent ? 'bg-success' : 'bg-muted-foreground/40',
                        )}
                      />
                      {i < chain.length - 1 && <span className="mt-0.5 w-px flex-1 bg-border" />}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="mb-1 flex flex-wrap items-center gap-2">
                        {isCurrent ? (
                          <Badge variant="secondary" className="bg-success/15 text-success">
                            {intl.formatMessage({ id: 'memory.history.current' })}
                          </Badge>
                        ) : (
                          <Badge variant="secondary">
                            {intl.formatMessage({ id: 'memory.history.superseded' })}
                          </Badge>
                        )}
                        {c.confidence != null && (
                          <span className="text-xs text-muted-foreground">
                            {intl.formatMessage(
                              { id: 'memory.history.confidence' },
                              { value: Math.round(c.confidence * 100) },
                            )}
                          </span>
                        )}
                      </div>
                      <p className="whitespace-pre-wrap text-sm text-foreground">{c.content}</p>
                      <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
                        <span className="flex items-center gap-1">
                          <ClockIcon className="size-3" />
                          {intl.formatMessage({ id: 'memory.history.validFrom' })}{' '}
                          <span className="font-mono">
                            {c.valid_from ? new Date(c.valid_from).toLocaleString() : '—'}
                          </span>
                        </span>
                        <span>
                          {intl.formatMessage({ id: 'memory.history.validUntil' })}{' '}
                          <span className="font-mono">
                            {c.valid_until
                              ? new Date(c.valid_until).toLocaleString()
                              : intl.formatMessage({ id: 'memory.history.now' })}
                          </span>
                        </span>
                      </p>
                    </div>
                  </li>
                );
              })}
            </ol>

            {subject && predicate && (
              <div className="mt-3 border-t border-surface-border pt-3">
                <p className="mb-2 text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'memory.history.pit.title' })}
                </p>
                <div className="flex flex-wrap items-center gap-2">
                  <Input
                    type="datetime-local"
                    value={atInput}
                    onChange={(e) => setAtInput(e.target.value)}
                    className="h-8 w-auto text-xs"
                    aria-label={intl.formatMessage({ id: 'memory.history.pit.title' })}
                  />
                  <Button variant="secondary" size="sm" onClick={handleAtQuery} disabled={atLoading || !atInput}>
                    {atLoading
                      ? intl.formatMessage({ id: 'common.loading' })
                      : intl.formatMessage({ id: 'memory.history.pit.query' })}
                  </Button>
                </div>
                {atResult && (
                  <div className="mt-2 rounded-lg bg-muted px-3 py-2">
                    {atResult.found && atResult.record ? (
                      <>
                        <p className="whitespace-pre-wrap text-sm text-foreground">
                          {atResult.record.content}
                        </p>
                        <p className="mt-1 text-xs text-muted-foreground">
                          {intl.formatMessage({ id: 'memory.history.validFrom' })}{' '}
                          <span className="font-mono">
                            {atResult.record.valid_from
                              ? new Date(atResult.record.valid_from).toLocaleString()
                              : '—'}
                          </span>
                        </p>
                      </>
                    ) : (
                      <p className="text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'memory.history.pit.none' })}
                      </p>
                    )}
                  </div>
                )}
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
