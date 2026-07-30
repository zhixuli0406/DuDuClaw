import { useState, useCallback, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { cn } from '@/lib/utils';
import {
  api,
  type MemoryEntry,
  type MemoryChainEntry,
  type MemoryAtRecord,
} from '@/lib/api';
import { parsePredictionMemory, toPercent, type PredictionMemory } from '@/lib/memory-format';
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
  ChevronRightIcon,
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
 * MemoryBrowser — the "記憶" surface, laid out the way the 2026-07-30 client
 * feedback asked for (Perplexity's memory page as the reference): a category
 * rail on the left, category cards on the right, and a hover-revealed delete
 * on every row.
 *
 * Grouping is deterministic and local (see `lib/memory-category.ts`) — the
 * backend has no topic field and adding an LLM pass per entry would cost on
 * every page view. Deleting is a soft delete on the backend (`memory.forget`
 * archives the row), so a mis-click is recoverable by an operator, but the UI
 * still asks for a second click before firing.
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

/** How many entries a category card previews before "show all". */
const CARD_PREVIEW = 5;

export function MemoryBrowser({ agentId, query }: { agentId: string; query: string }) {
  const intl = useIntl();
  const [entries, setEntries] = useState<ReadonlyArray<MemoryEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<MemoryCategoryId | 'all'>('all');
  // Cognitive memory is ON by default — the empty state must not tell users to
  // "enable" something already running. `false` (explicitly disabled in agent
  // settings) selects the disabled-variant copy; `null` = unknown/loading.
  const [cognitiveMemory, setCognitiveMemory] = useState<boolean | null>(null);

  // Browse on agent change.
  useEffect(() => {
    if (!agentId) return;
    setLoading(true);
    setSelected('all');
    api.memory.browse(agentId, 200).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    }).finally(() => setLoading(false));
    setCognitiveMemory(null);
    api.agents.inspect(agentId).then((detail) => {
      setCognitiveMemory(detail?.evolution?.cognitive_memory ?? null);
    }).catch(() => {
      // Best-effort — the empty state just falls back to the default copy.
      setCognitiveMemory(null);
    });
  }, [agentId, intl]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !agentId) return;
    setLoading(true);
    try {
      const result = await api.memory.search(agentId, query, 200);
      setEntries(result?.entries ?? []);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
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

  /** Drop a forgotten entry from local state — no refetch, no scroll jump. */
  const handleForgotten = useCallback((id: string) => {
    setEntries((prev) => prev.filter((e) => e.id !== id));
  }, []);

  if (loading) return <MemoryListSkeleton />;

  if (entries.length === 0) {
    if (query.trim()) {
      return (
        <CollectionPageState
          state="empty"
          icon={BrainIcon}
          title={intl.formatMessage({ id: 'memory.empty.search' }, { query })}
        />
      );
    }
    // Two empty-state variants: cognitive memory explicitly disabled → explain
    // and link to settings; otherwise (default-on) → explain that substantive
    // conversations auto-distill here, with NO enable call-to-action.
    const disabled = cognitiveMemory === false;
    return (
      <CollectionPageState
        state="empty"
        icon={BrainIcon}
        title={intl.formatMessage({
          id: disabled ? 'memory.empty.memories.disabled' : 'memory.empty.memories',
        })}
        action={
          disabled ? (
            <Link
              to="/agents"
              className="inline-flex items-center gap-1.5 text-sm font-medium text-brand hover:underline"
            >
              {intl.formatMessage({ id: 'memory.empty.memories.action' })}
              <ArrowRightIcon className="size-3.5" />
            </Link>
          ) : undefined
        }
      />
    );
  }

  const activeBucket = selected === 'all' ? null : buckets.find((b) => b.category.id === selected);

  return (
    <div className="flex flex-col gap-4 md:flex-row md:items-start md:gap-6">
      <CategoryRail
        buckets={buckets}
        total={entries.length}
        selected={selected}
        onSelect={setSelected}
      />
      <div className="min-w-0 flex-1">
        {selected === 'all' ? (
          <div className="grid gap-3 lg:grid-cols-2 2xl:grid-cols-3">
            {buckets.map((bucket) => (
              <CategoryCard
                key={bucket.category.id}
                bucket={bucket}
                onShowAll={() => setSelected(bucket.category.id)}
                onForgotten={handleForgotten}
              />
            ))}
          </div>
        ) : activeBucket ? (
          <div className="flex flex-col gap-1.5">
            <h2 className="flex items-center gap-2 px-2 pb-1 text-sm font-medium text-foreground">
              <CategoryIcon name={activeBucket.category.icon} className="size-4 text-brand" />
              {intl.formatMessage({ id: activeBucket.category.label })}
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {activeBucket.entries.length}
              </span>
            </h2>
            {activeBucket.entries.map((entry) => (
              <MemoryItem key={entry.id} entry={entry} onForgotten={handleForgotten} />
            ))}
          </div>
        ) : (
          <CollectionPageState
            state="empty"
            icon={BrainIcon}
            title={intl.formatMessage({ id: 'memory.category.empty' })}
          />
        )}
      </div>
    </div>
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

/** One category as a card: a few entries plus a "show all" affordance. */
function CategoryCard({
  bucket,
  onShowAll,
  onForgotten,
}: {
  bucket: CategoryBucket<MemoryEntry>;
  onShowAll: () => void;
  onForgotten: (id: string) => void;
}) {
  const intl = useIntl();
  const preview = bucket.entries.slice(0, CARD_PREVIEW);
  const rest = bucket.entries.length - preview.length;
  return (
    <Card data-size="sm" className="h-full">
      <CardContent className="space-y-1">
        <div className="flex items-center gap-2 pb-1">
          <CategoryIcon name={bucket.category.icon} className="size-4 shrink-0 text-brand" />
          <h3 className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
            {intl.formatMessage({ id: bucket.category.label })}
          </h3>
          <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
            {bucket.entries.length}
          </span>
        </div>
        {preview.map((entry) => (
          <MemoryItem key={entry.id} entry={entry} onForgotten={onForgotten} compact />
        ))}
        {rest > 0 && (
          <button
            type="button"
            onClick={onShowAll}
            className="flex items-center gap-1 px-2 pt-1 text-xs font-medium text-brand hover:underline"
          >
            {intl.formatMessage({ id: 'memory.category.showAll' }, { count: rest })}
            <ChevronRightIcon className="size-3" />
          </button>
        )}
      </CardContent>
    </Card>
  );
}

/**
 * A memory entry. Prediction-deviation telemetry renders as a "learning
 * signal" card; everything else is a slim row with an expandable supersession
 * history and a hover-revealed delete.
 */
function MemoryItem({
  entry,
  onForgotten,
  compact = false,
}: {
  entry: MemoryEntry;
  onForgotten: (id: string) => void;
  compact?: boolean;
}) {
  const intl = useIntl();
  const prediction = parsePredictionMemory(entry.content);
  if (prediction) {
    if (!compact) {
      return <PredictionMemoryCard entry={entry} data={prediction} onForgotten={onForgotten} />;
    }
    // Inside a category card there is no room for the full card, but the raw
    // English telemetry string must never reach the user either — summarize it.
    const summary = `${intl.formatMessage({ id: 'memory.prediction.label' })} · ${toPercent(
      prediction.expected,
    )}% → ${toPercent(prediction.inferred)}%`;
    return <MemoryRow entry={entry} onForgotten={onForgotten} compact displayText={summary} />;
  }
  return <MemoryRow entry={entry} onForgotten={onForgotten} compact={compact} />;
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

/** A single memory entry rendered as a slim row with an expandable history. */
function MemoryRow({
  entry,
  onForgotten,
  compact = false,
  displayText,
}: {
  entry: MemoryEntry;
  onForgotten: (id: string) => void;
  compact?: boolean;
  /** Overrides the row text (used to humanize telemetry-shaped content). */
  displayText?: string;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const text = displayText ?? entry.content;
  return (
    <div className="group rounded-lg border border-transparent transition-colors hover:border-surface-border hover:bg-accent/30">
      <div className="flex h-9 items-center gap-2 px-2">
        <BrainIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-sm text-foreground" title={text}>
          {text}
        </span>
        {!compact && entry.tags[0] && (
          <Badge variant="secondary" className="hidden shrink-0 sm:inline-flex">
            {entry.tags[0]}
          </Badge>
        )}
        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
          {timeAgo(entry.timestamp)}
        </span>
        <ForgetButton entry={entry} onForgotten={onForgotten} />
        {!compact && (
          <Button
            variant="ghost"
            size="icon-xs"
            aria-expanded={open}
            aria-label={intl.formatMessage({ id: 'memory.history.toggle' })}
            onClick={() => setOpen((v) => !v)}
            className="shrink-0"
          >
            {open ? <ChevronUpIcon /> : <ChevronDownIcon />}
          </Button>
        )}
      </div>
      {open && (
        <div className="px-2 pb-2">
          <MemoryHistory agentId={entry.agent_id} memoryId={entry.id} />
        </div>
      )}
    </div>
  );
}

/**
 * Renders a prediction-deviation episodic memory as a human-readable "learning
 * signal" card instead of the raw English telemetry string (which users can't
 * parse). See {@link parsePredictionMemory}.
 */
function PredictionMemoryCard({
  entry,
  data,
  onForgotten,
}: {
  entry: MemoryEntry;
  data: PredictionMemory;
  onForgotten: (id: string) => void;
}) {
  const intl = useIntl();
  const expectedPct = toPercent(data.expected);
  const actualPct = toPercent(data.inferred);
  const lower = data.inferred < data.expected;

  return (
    <Card data-size="sm" className="group">
      <CardContent className="space-y-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="flex min-w-0 items-center gap-1.5 text-xs font-medium text-brand">
            <ActivityIcon className="size-3.5 shrink-0" />
            <span className="truncate">{entry.agent_id}</span>
            <span className="text-muted-foreground">
              · {intl.formatMessage({ id: 'memory.prediction.label' })}
            </span>
          </span>
          <span className="flex shrink-0 items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
            <ClockIcon className="size-3" />
            {timeAgo(entry.timestamp)}
            <ForgetButton entry={entry} onForgotten={onForgotten} />
          </span>
        </div>
        <p className="text-sm text-foreground">
          {intl.formatMessage({ id: 'memory.prediction.satisfaction' })}{' '}
          <span className="tabular-nums text-muted-foreground">{expectedPct}%</span>
          <ArrowRightIcon className="mx-1 inline size-3 text-muted-foreground" />
          <span className={cn('font-medium tabular-nums', lower ? 'text-destructive' : 'text-success')}>
            {actualPct}%
          </span>
        </p>
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
      </CardContent>
    </Card>
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
