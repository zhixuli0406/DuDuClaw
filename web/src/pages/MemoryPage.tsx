import { useState, useCallback, useEffect } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type MemoryEntry,
  type EvolutionVersion,
  type KeyFactEntry,
  type MemoryChainEntry,
  type MemoryAtRecord,
} from '@/lib/api';
import { parsePredictionMemory, toPercent, type PredictionMemory } from '@/lib/memory-format';
import { timeAgo } from '@/lib/format';
import { Link } from 'react-router';
import { toast, formatError } from '@/lib/toast';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Segmented,
  Button,
  Badge,
  Input,
  Skeleton,
  ActorAvatar,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  type SegmentedOption,
} from '@/components/mds';
import {
  BrainIcon,
  SearchIcon,
  ClockIcon,
  GitBranchIcon,
  CheckCircleIcon,
  XCircleIcon,
  ArrowRightIcon,
  LightbulbIcon,
  ActivityIcon,
  HistoryIcon,
  ChevronDownIcon,
  ChevronUpIcon,
} from 'lucide-react';

type ViewId = 'memories' | 'insights' | 'evolution';

/**
 * MemoryPage — the Multica "記憶" collection (spec §4/§5.2). A
 * CollectionPageHeader + a Segmented view switcher (memories / key insights /
 * self-improvement) with an agent picker and — on the memories view — a search
 * field on the control row. Memory entries render as slim list rows; prediction-
 * deviation "learning signal" entries and the temporal supersession chain render
 * as MDS Cards. Data flow (browse / search / key facts / history / evolution
 * status) is unchanged; only the surface is re-skinned onto MDS.
 */
export function MemoryPage() {
  const intl = useIntl();
  const [view, setView] = useState<ViewId>('memories');
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');
  const [query, setQuery] = useState('');

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) setSelectedAgent((prev) => prev || list[0].name);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; intl is stable from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const viewOptions: SegmentedOption<ViewId>[] = [
    { value: 'memories', label: intl.formatMessage({ id: 'memory.tab.memories' }) },
    { value: 'insights', label: intl.formatMessage({ id: 'memory.tab.insights' }) },
    { value: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }) },
  ];

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={BrainIcon}
        title={intl.formatMessage({ id: 'nav.memory' })}
        description={intl.formatMessage({ id: 'nav.memory.desc' })}
      />

      {/* Control row: view switcher + agent picker + (memories) search. */}
      <div className="flex h-12 shrink-0 items-center gap-2 overflow-x-auto border-b border-surface-border px-4">
        <Segmented
          value={view}
          onValueChange={setView}
          options={viewOptions}
          aria-label={intl.formatMessage({ id: 'nav.memory' })}
        />
        {view !== 'evolution' && (
          <AgentSelect
            className="ml-auto"
            value={selectedAgent}
            onValueChange={setSelectedAgent}
            agents={agents}
          />
        )}
        {view === 'memories' && (
          <div className="relative shrink-0">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={intl.formatMessage({ id: 'memory.search.placeholder' })}
              className="w-40 pl-8 sm:w-56"
            />
          </div>
        )}
      </div>

      <div className="flex flex-1 flex-col p-4 md:p-6">
        {view === 'memories' && <MemoriesView agentId={selectedAgent} query={query} />}
        {view === 'insights' && <InsightsView agentId={selectedAgent} />}
        {view === 'evolution' && <EvolutionView />}
      </div>
    </div>
  );
}

/** Small agent picker shared by the memory views. */
function AgentSelect({
  value,
  onValueChange,
  agents,
  className,
}: {
  value: string;
  onValueChange: (v: string) => void;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  className?: string;
}) {
  const current = agents.find((a) => a.name === value);
  if (agents.length === 0) return null;
  return (
    <Select value={value} onValueChange={(v) => onValueChange(String(v))}>
      <SelectTrigger className={cn('w-44 shrink-0', className)}>
        <SelectValue>{current ? current.display_name || current.name : value}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {agents.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {a.display_name || a.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

// ── Memories view (browse + search, slim rows) ──────────────

function MemoriesView({ agentId, query }: { agentId: string; query: string }) {
  const intl = useIntl();
  const [entries, setEntries] = useState<ReadonlyArray<MemoryEntry>>([]);
  const [loading, setLoading] = useState(false);

  // Browse on agent change.
  useEffect(() => {
    if (!agentId) return;
    setLoading(true);
    api.memory.browse(agentId, 50).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    }).finally(() => setLoading(false));
  }, [agentId, intl]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !agentId) return;
    setLoading(true);
    try {
      const result = await api.memory.search(agentId, query);
      setEntries(result?.entries ?? []);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [query, agentId, intl]);

  // Debounced search when the query changes (Enter-less UX; the control-row
  // Input lives on the page, so we react to `query` here).
  useEffect(() => {
    if (!query.trim()) return;
    const t = setTimeout(() => void handleSearch(), 350);
    return () => clearTimeout(t);
  }, [query, handleSearch]);

  if (loading) return <MemoryListSkeleton />;

  if (entries.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={BrainIcon}
        title={intl.formatMessage({ id: 'memory.empty.memories' })}
        action={
          <Link
            to="/agents"
            className="inline-flex items-center gap-1.5 text-sm font-medium text-brand hover:underline"
          >
            {intl.formatMessage({ id: 'memory.empty.memories.action' })}
            <ArrowRightIcon className="size-3.5" />
          </Link>
        }
      />
    );
  }

  return (
    <div className="flex flex-col gap-1.5">
      {entries.map((entry) => {
        const prediction = parsePredictionMemory(entry.content);
        if (prediction) {
          return <PredictionMemoryCard key={entry.id} entry={entry} data={prediction} />;
        }
        return <MemoryRow key={entry.id} entry={entry} />;
      })}
    </div>
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

/** A single memory entry rendered as a slim row with an expandable history. */
function MemoryRow({ entry }: { entry: MemoryEntry }) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-lg border border-transparent transition-colors hover:border-surface-border hover:bg-accent/30">
      <div className="flex h-9 items-center gap-2.5 px-2">
        <BrainIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-sm text-foreground" title={entry.content}>
          {entry.content}
        </span>
        {entry.tags[0] && (
          <Badge variant="secondary" className="hidden shrink-0 sm:inline-flex">
            {entry.tags[0]}
          </Badge>
        )}
        <ActorAvatar actorType="agent" size="xs" name={entry.agent_id} />
        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
          {timeAgo(entry.timestamp)}
        </span>
        <Button
          variant="ghost"
          size="icon-xs"
          aria-expanded={open}
          aria-label={intl.formatMessage({ id: 'memory.history.toggle' })}
          onClick={() => setOpen((v) => !v)}
        >
          {open ? <ChevronUpIcon /> : <ChevronDownIcon />}
        </Button>
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
function PredictionMemoryCard({ entry, data }: { entry: MemoryEntry; data: PredictionMemory }) {
  const intl = useIntl();
  const expectedPct = toPercent(data.expected);
  const actualPct = toPercent(data.inferred);
  const lower = data.inferred < data.expected;

  return (
    <Card data-size="sm">
      <CardContent className="space-y-2.5">
        <div className="flex items-center justify-between gap-2">
          <span className="flex min-w-0 items-center gap-1.5 text-xs font-medium text-brand">
            <ActorAvatar actorType="agent" size="xs" name={entry.agent_id} />
            <ActivityIcon className="size-3.5 shrink-0" />
            <span className="truncate">{entry.agent_id}</span>
            <span className="text-muted-foreground">
              · {intl.formatMessage({ id: 'memory.prediction.label' })}
            </span>
          </span>
          <span className="flex shrink-0 items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
            <ClockIcon className="size-3" />
            {timeAgo(entry.timestamp)}
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

// ── Insights view (key facts) ───────────────────────────────

function InsightsView({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [facts, setFacts] = useState<ReadonlyArray<KeyFactEntry>>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!agentId) return;
    setLoading(true);
    api.memory.keyFacts(agentId, 50).then((res) => {
      setFacts(res?.entries ?? []);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setFacts([]);
    }).finally(() => setLoading(false));
  }, [agentId, intl]);

  if (loading) return <MemoryListSkeleton />;

  if (facts.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={LightbulbIcon}
        title={intl.formatMessage({ id: 'memory.empty.insights' })}
      />
    );
  }

  return (
    <div className="flex flex-col gap-2.5">
      {facts.map((fact) => (
        <Card key={fact.id} data-size="sm">
          <CardContent className="space-y-2">
            <div className="flex items-start justify-between gap-3">
              <div className="flex min-w-0 items-center gap-2">
                <ActorAvatar actorType="agent" size="xs" name={fact.agent_id} />
                <LightbulbIcon className="size-4 shrink-0 text-brand" />
                <span className="truncate text-xs font-medium text-brand">{fact.agent_id}</span>
                {fact.access_count > 0 && (
                  <Badge variant="secondary" className="bg-info/15 text-info">
                    {intl.formatMessage({ id: 'memory.insights.accessCount' }, { count: fact.access_count })}
                  </Badge>
                )}
              </div>
              <span className="flex shrink-0 items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
                <ClockIcon className="size-3" />
                {timeAgo(fact.timestamp)}
              </span>
            </div>
            <p className="whitespace-pre-wrap text-sm text-foreground">{fact.fact}</p>
            {(fact.source_session || fact.channel || fact.chat_id) && (
              <div className="flex flex-wrap gap-x-3 gap-y-1 border-t border-surface-border pt-2 text-xs text-muted-foreground">
                {fact.source_session && (
                  <span>session: <span className="font-mono">{fact.source_session}</span></span>
                )}
                {fact.channel && <span>channel: <span className="font-mono">{fact.channel}</span></span>}
                {fact.chat_id && <span>chat: <span className="font-mono">{fact.chat_id}</span></span>}
              </div>
            )}
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

// ── Evolution view (self-improvement) ───────────────────────

interface EvolutionAgent {
  agent_id: string;
  gvu_enabled: boolean;
  cognitive_memory: boolean;
  skill_auto_activate: boolean;
  skill_security_scan: boolean;
  max_silence_hours: number;
  max_gvu_generations: number;
  observation_period_hours: number;
}

function EvolutionView() {
  const intl = useIntl();
  const [agents, setAgents] = useState<EvolutionAgent[]>([]);
  const [mode, setMode] = useState('');
  const [enabled, setEnabled] = useState(false);
  const [gvuEnabledCount, setGvuEnabledCount] = useState(0);
  const [totalVersions, setTotalVersions] = useState(0);
  const [lastAppliedAt, setLastAppliedAt] = useState<string | null>(null);
  const [versions, setVersions] = useState<ReadonlyArray<EvolutionVersion>>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    setLoading(true);
    let notified = false;
    const onFailure = (e: unknown) => {
      console.warn('[api]', e);
      if (notified) return null;
      notified = true;
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      return null;
    };
    Promise.all([
      api.evolution.status().catch(onFailure),
      api.evolution.history(undefined, 20).catch(onFailure),
    ]).then(([status, history]) => {
      setAgents(status?.agents ?? []);
      setMode(status?.mode ?? '');
      setEnabled(status?.enabled ?? false);
      setGvuEnabledCount(status?.gvu_enabled_count ?? 0);
      setTotalVersions(status?.total_versions ?? 0);
      setLastAppliedAt(status?.last_applied_at ?? null);
      setVersions(history?.versions ?? []);
    }).finally(() => setLoading(false));
  }, [intl]);

  if (loading) return <CollectionPageState state="loading" />;

  return (
    <div className="space-y-4">
      {mode && (
        <Card data-size="sm" className={cn(enabled && 'border-brand/40')}>
          <CardContent className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="flex items-center gap-2">
              <GitBranchIcon className={cn('size-4', enabled ? 'text-brand' : 'text-muted-foreground')} />
              <span className={cn('text-sm', enabled ? 'text-foreground' : 'text-muted-foreground')}>
                {intl.formatMessage({ id: 'evolution.mode' })}:{' '}
                <span className="font-medium">{mode.replace('_', ' ')}</span>
              </span>
            </span>
            <span className="text-xs text-muted-foreground">
              {gvuEnabledCount}/{agents.length} {intl.formatMessage({ id: 'evolution.agentsEnabled' })}
            </span>
            {totalVersions > 0 && (
              <span className="text-xs text-muted-foreground">· {totalVersions} versions</span>
            )}
            {lastAppliedAt && (
              <span className="flex items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
                <ClockIcon className="size-3" />
                {timeAgo(lastAppliedAt)}
              </span>
            )}
          </CardContent>
        </Card>
      )}

      {agents.length === 0 ? (
        <CollectionPageState state="empty" icon={GitBranchIcon} title={intl.formatMessage({ id: 'common.noData' })} />
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <Card key={agent.agent_id} data-size="sm">
              <CardContent className="space-y-3">
                <div className="flex items-center gap-2">
                  <ActorAvatar actorType="agent" size="sm" name={agent.agent_id} />
                  <h3 className="truncate text-sm font-medium text-foreground">{agent.agent_id}</h3>
                </div>
                <div className="space-y-2 text-sm">
                  <EvolutionRow label="GVU" enabled={agent.gvu_enabled} />
                  <EvolutionRow
                    label={intl.formatMessage({ id: 'agents.edit.cognitiveMemory' })}
                    enabled={agent.cognitive_memory}
                  />
                  <EvolutionRow
                    label={intl.formatMessage({ id: 'agents.edit.skillAutoActivate' })}
                    enabled={agent.skill_auto_activate}
                  />
                  <EvolutionRow
                    label={intl.formatMessage({ id: 'agents.edit.skillSecurityScan' })}
                    enabled={agent.skill_security_scan}
                  />
                </div>
                <div className="grid grid-cols-3 gap-2 border-t border-surface-border pt-3">
                  <Metric value={String(agent.max_gvu_generations)} label={intl.formatMessage({ id: 'evolution.maxGenerations' })} />
                  <Metric value={`${agent.observation_period_hours}h`} label={intl.formatMessage({ id: 'evolution.observationPeriod' })} />
                  <Metric value={`${agent.max_silence_hours}h`} label={intl.formatMessage({ id: 'evolution.maxSilence' })} />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {agents.length > 0 && (
        <section className="space-y-2">
          <h2 className="flex items-center gap-2 text-sm font-medium text-foreground">
            <GitBranchIcon className="size-4 text-brand" />
            {intl.formatMessage({ id: 'evolution.engine' })}
          </h2>
          {versions.length === 0 ? (
            <CollectionPageState
              state="empty"
              icon={GitBranchIcon}
              title={intl.formatMessage({ id: 'evolution.noHistory' })}
            />
          ) : (
            <div className="space-y-2">
              {versions.map((v) => (
                <EvolutionVersionCard key={v.version_id} version={v} />
              ))}
            </div>
          )}
        </section>
      )}
    </div>
  );
}

function Metric({ value, label }: { value: string; label: string }) {
  return (
    <div className="text-center">
      <p className="font-mono text-lg font-medium tabular-nums text-foreground">{value}</p>
      <p className="text-xs text-muted-foreground">{label}</p>
    </div>
  );
}

function EvolutionVersionCard({ version }: { version: EvolutionVersion }) {
  const intl = useIntl();

  const statusLabel = (() => {
    switch (version.status) {
      case 'Confirmed': return intl.formatMessage({ id: 'evolution.status.confirmed' });
      case 'RolledBack': return intl.formatMessage({ id: 'evolution.status.rolledBack' });
      case 'Observing': return intl.formatMessage({ id: 'evolution.status.observing' });
      default: return version.status;
    }
  })();
  const statusClass: Record<string, string> = {
    Confirmed: 'bg-success/15 text-success',
    RolledBack: 'bg-destructive/10 text-destructive',
    Observing: 'bg-warning/15 text-warning',
  };

  const renderDelta = (pre: number, post: number | undefined, invert = false) => {
    if (post === undefined || post === null) {
      return <span className="text-muted-foreground">{pre.toFixed(2)}</span>;
    }
    const delta = post - pre;
    const good = invert ? delta < 0 : delta > 0;
    const color = Math.abs(delta) < 1e-6
      ? 'text-muted-foreground'
      : good ? 'text-success' : 'text-destructive';
    return (
      <span>
        <span className="text-muted-foreground">{pre.toFixed(2)}</span>
        <span className="mx-1 text-muted-foreground">→</span>
        <span className={color}>{post.toFixed(2)}</span>
      </span>
    );
  };

  return (
    <Card data-size="sm">
      <CardContent className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <ActorAvatar actorType="agent" size="xs" name={version.agent_id} />
            <span className="text-sm font-medium text-brand">{version.agent_id}</span>
            <Badge variant="secondary" className={statusClass[version.status]}>{statusLabel}</Badge>
            <span className="font-mono text-xs text-muted-foreground">{version.soul_hash.slice(0, 8)}</span>
          </div>
          <span className="flex items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
            <ClockIcon className="size-3" />
            {timeAgo(version.applied_at)}
          </span>
        </div>
        {version.soul_summary && (
          <p className="whitespace-pre-wrap text-sm text-foreground">{version.soul_summary}</p>
        )}
        <div className="grid grid-cols-3 gap-2 border-t border-surface-border pt-2 text-xs">
          <div>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'evolution.metric.feedback' })}</p>
            <p className="font-mono">
              {renderDelta(version.pre_metrics.positive_feedback_ratio, version.post_metrics?.positive_feedback_ratio)}
            </p>
          </div>
          <div>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'evolution.metric.error' })}</p>
            <p className="font-mono">
              {renderDelta(version.pre_metrics.prediction_error, version.post_metrics?.prediction_error, true)}
            </p>
          </div>
          <div>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'evolution.metric.corrections' })}</p>
            <p className="font-mono">
              {renderDelta(version.pre_metrics.user_correction_rate, version.post_metrics?.user_correction_rate, true)}
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function EvolutionRow({ label, enabled }: { label: string; enabled: boolean }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-muted-foreground">{label}</span>
      {enabled ? (
        <CheckCircleIcon className="size-4 text-success" />
      ) : (
        <XCircleIcon className="size-4 text-muted-foreground/40" />
      )}
    </div>
  );
}
