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
import { Link } from 'react-router';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Tabs,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  controlClass,
  CharacterAvatar,
  Mono,
  type TabItem,
} from '@/components/ui';
import {
  Brain,
  Search,
  Tag,
  Clock,
  GitBranch,
  CheckCircle,
  XCircle,
  ArrowRight,
  Lightbulb,
  Activity,
  History,
  ChevronDown,
  ChevronUp,
} from 'lucide-react';

type TabId = 'memories' | 'evolution' | 'insights';

export function MemoryPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('memories');

  const tabs: TabItem[] = [
    { id: 'memories', label: intl.formatMessage({ id: 'memory.tab.memories' }), icon: Brain },
    { id: 'insights', label: intl.formatMessage({ id: 'memory.tab.insights' }), icon: Lightbulb },
    { id: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }), icon: GitBranch },
  ];

  return (
    <Page wide>
      <PageHeader
        icon={Brain}
        title={intl.formatMessage({ id: 'nav.memory' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
      />

      <Tabs items={tabs} value={activeTab} onChange={(id) => setActiveTab(id as TabId)} />

      {activeTab === 'memories' && <MemoriesTab />}
      {activeTab === 'insights' && <InsightsTab />}
      {activeTab === 'evolution' && <EvolutionTab />}
    </Page>
  );
}

function MemoriesTab() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [entries, setEntries] = useState<ReadonlyArray<MemoryEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  // Load agent list on mount
  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0 && !selectedAgent) {
        setSelectedAgent(list[0].name);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; `selectedAgent` is only read inside the promise body
    // to seed a default and shouldn't retrigger this fetch. `intl` is stable
    // from react-intl context, so omitting it here is safe.
  }, []);

  // Auto-browse when agent changes
  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    api.memory.browse(selectedAgent, 50).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    }).finally(() => setLoading(false));
  }, [selectedAgent, intl]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !selectedAgent) return;
    setLoading(true);
    try {
      const result = await api.memory.search(selectedAgent, query);
      setEntries(result?.entries ?? []);
    } catch (e) {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [query, selectedAgent, intl]);

  return (
    <div className="space-y-4">
      {/* Agent selector + Search bar */}
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        onSearchEnter={handleSearch}
        searchPlaceholder={intl.formatMessage({ id: 'memory.search.placeholder' })}
      >
        <select
          value={selectedAgent}
          onChange={(e) => { setSelectedAgent(e.target.value); setQuery(''); }}
          className={cn(controlClass, 'w-auto')}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
        <Button
          variant="primary"
          icon={Search}
          onClick={handleSearch}
          disabled={loading || !selectedAgent}
          aria-label={intl.formatMessage({ id: 'memory.search.placeholder' })}
        />
      </Toolbar>

      {/* Memory entries */}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : entries.length === 0 ? (
        <Card>
          <EmptyState
            icon={Brain}
            dudu="idle"
            title={intl.formatMessage({ id: 'memory.empty.memories' })}
            action={
              <Link
                to="/agents"
                className="inline-flex items-center gap-1.5 text-sm font-medium text-amber-600 hover:text-amber-700 dark:text-amber-400 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'memory.empty.memories.action' })}
                <ArrowRight className="h-3.5 w-3.5" />
              </Link>
            }
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {entries.map((entry) => {
            const prediction = parsePredictionMemory(entry.content);
            if (prediction) {
              return <PredictionMemoryCard key={entry.id} entry={entry} data={prediction} />;
            }
            return (
              <Card key={entry.id}>
                <div className="mb-2 flex items-center justify-between">
                  <span className="flex items-center gap-1.5 text-xs font-medium text-amber-600 dark:text-amber-400">
                    <CharacterAvatar agentId={entry.agent_id} name={entry.agent_id} size={24} />
                    {entry.agent_id}
                  </span>
                  <span className="flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
                    <Clock className="h-3 w-3" />
                    <Mono className="text-stone-400 dark:text-stone-500">{new Date(entry.timestamp).toLocaleString()}</Mono>
                  </span>
                </div>
                <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
                  {entry.content}
                </p>
                {entry.tags.length > 0 && (
                  <div className="mt-3 flex flex-wrap gap-1.5">
                    {entry.tags.map((tag) => (
                      <Badge key={tag} tone="neutral">
                        <Tag className="h-2.5 w-2.5" />
                        {tag}
                      </Badge>
                    ))}
                  </div>
                )}
                <MemoryHistory agentId={entry.agent_id} memoryId={entry.id} />
              </Card>
            );
          })}
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
    <Card>
      <div className="mb-2 flex items-center justify-between">
        <span className="flex items-center gap-1.5 text-xs font-medium text-amber-600 dark:text-amber-400">
          <CharacterAvatar agentId={entry.agent_id} name={entry.agent_id} size={24} />
          <Activity className="h-3.5 w-3.5" />
          {entry.agent_id}
          <span className="text-stone-400 dark:text-stone-500">
            · {intl.formatMessage({ id: 'memory.prediction.label' })}
          </span>
        </span>
        <span className="flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
          <Clock className="h-3 w-3" />
          <Mono className="text-stone-400 dark:text-stone-500">{new Date(entry.timestamp).toLocaleString()}</Mono>
        </span>
      </div>
      <p className="text-sm text-stone-700 dark:text-stone-300">
        {intl.formatMessage({ id: 'memory.prediction.satisfaction' })}{' '}
        <span className="text-stone-500 dark:text-stone-400 tabular-nums">{expectedPct}%</span>
        <ArrowRight className="mx-1 inline h-3 w-3 text-stone-400" />
        <span
          className={cn(
            'font-medium tabular-nums',
            lower ? 'text-rose-600 dark:text-rose-400' : 'text-emerald-600 dark:text-emerald-400',
          )}
        >
          {actualPct}%
        </span>
      </p>
      <div className="mt-2.5 flex flex-wrap gap-1.5">
        <Badge tone="neutral">
          {intl.formatMessage({ id: 'memory.prediction.surprise' }, { value: toPercent(data.surprise) })}
        </Badge>
        {data.corrected && (
          <Badge tone="warning">{intl.formatMessage({ id: 'memory.prediction.corrected' })}</Badge>
        )}
        {data.followUp && (
          <Badge tone="accent">{intl.formatMessage({ id: 'memory.prediction.followUp' })}</Badge>
        )}
      </div>
      <p className="mt-2.5 text-xs text-stone-400 dark:text-stone-500">
        {intl.formatMessage({ id: 'memory.prediction.note' })}
      </p>
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
  const [open, setOpen] = useState(false);
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

  const load = useCallback(async () => {
    setLoading(true);
    setFailed(false);
    try {
      const res = await api.memory.history(agentId, { memory_id: memoryId });
      setChain(res?.chain ?? []);
      setCurrentId(res?.current_id ?? null);
      setSubject(res?.subject ?? '');
      setPredicate(res?.predicate ?? '');
      setLoaded(true);
    } catch (e) {
      console.warn('[api]', e);
      setFailed(true);
    } finally {
      setLoading(false);
    }
  }, [agentId, memoryId]);

  const handleToggle = () => {
    const next = !open;
    setOpen(next);
    if (next && !loaded && !loading) void load();
  };

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
    <div className="mt-3 border-t border-[var(--panel-border)] pt-2.5">
      <button
        type="button"
        onClick={handleToggle}
        aria-expanded={open}
        className="flex items-center gap-1.5 text-xs font-medium text-stone-500 hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
      >
        <History className="h-3.5 w-3.5" />
        {intl.formatMessage({ id: 'memory.history.toggle' })}
        {open ? <ChevronUp className="h-3 w-3" /> : <ChevronDown className="h-3 w-3" />}
      </button>

      {open && (
        <div className="mt-3">
          {loading ? (
            <p className="py-3 text-xs text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
          ) : failed ? (
            <p className="py-3 text-xs text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'memory.history.loadError' })}
            </p>
          ) : chain.length === 0 ? (
            <p className="py-3 text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'memory.history.empty' })}
            </p>
          ) : (
            <>
              {(subject || predicate) && (
                <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">
                  <Mono className="text-xs text-stone-500 dark:text-stone-400">{subject}</Mono>
                  {' · '}
                  <Mono className="text-xs text-stone-500 dark:text-stone-400">{predicate}</Mono>
                </p>
              )}
              <ol className="space-y-0">
                {chain.map((c, i) => {
                  const isCurrent = c.is_current || c.id === currentId;
                  return (
                    <li key={c.id} className="relative flex gap-3 pb-3 last:pb-0">
                      {/* timeline rail */}
                      <div className="relative flex flex-col items-center">
                        <span
                          className={cn(
                            'mt-1 h-2.5 w-2.5 shrink-0 rounded-full',
                            isCurrent ? 'bg-emerald-500' : 'bg-stone-300 dark:bg-stone-600',
                          )}
                        />
                        {i < chain.length - 1 && (
                          <span className="mt-0.5 w-px flex-1 bg-stone-200 dark:bg-stone-700" />
                        )}
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="mb-1 flex flex-wrap items-center gap-2">
                          {isCurrent ? (
                            <Badge tone="success">{intl.formatMessage({ id: 'memory.history.current' })}</Badge>
                          ) : (
                            <Badge tone="neutral">{intl.formatMessage({ id: 'memory.history.superseded' })}</Badge>
                          )}
                          {c.confidence != null && (
                            <span className="text-[11px] text-stone-400 dark:text-stone-500">
                              {intl.formatMessage(
                                { id: 'memory.history.confidence' },
                                { value: Math.round(c.confidence * 100) },
                              )}
                            </span>
                          )}
                        </div>
                        <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
                          {c.content}
                        </p>
                        <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-stone-400 dark:text-stone-500">
                          <span className="flex items-center gap-1">
                            <Clock className="h-3 w-3" />
                            {intl.formatMessage({ id: 'memory.history.validFrom' })}{' '}
                            <Mono className="text-[11px] text-stone-500 dark:text-stone-400">
                              {c.valid_from ? new Date(c.valid_from).toLocaleString() : '—'}
                            </Mono>
                          </span>
                          <span>
                            {intl.formatMessage({ id: 'memory.history.validUntil' })}{' '}
                            <Mono className="text-[11px] text-stone-500 dark:text-stone-400">
                              {c.valid_until
                                ? new Date(c.valid_until).toLocaleString()
                                : intl.formatMessage({ id: 'memory.history.now' })}
                            </Mono>
                          </span>
                        </p>
                      </div>
                    </li>
                  );
                })}
              </ol>

              {/* Point-in-time query */}
              {subject && predicate && (
                <div className="mt-3 border-t border-[var(--panel-border)] pt-3">
                  <p className="mb-2 text-[11px] font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'memory.history.pit.title' })}
                  </p>
                  <div className="flex flex-wrap items-center gap-2">
                    <input
                      type="datetime-local"
                      value={atInput}
                      onChange={(e) => setAtInput(e.target.value)}
                      className={cn(controlClass, 'h-8 w-auto text-xs')}
                      aria-label={intl.formatMessage({ id: 'memory.history.pit.title' })}
                    />
                    <Button
                      variant="secondary"
                      onClick={handleAtQuery}
                      disabled={atLoading || !atInput}
                    >
                      {atLoading
                        ? intl.formatMessage({ id: 'common.loading' })
                        : intl.formatMessage({ id: 'memory.history.pit.query' })}
                    </Button>
                  </div>
                  {atResult && (
                    <div className="mt-2 rounded-lg bg-stone-500/5 px-3 py-2 dark:bg-white/5">
                      {atResult.found && atResult.record ? (
                        <>
                          <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
                            {atResult.record.content}
                          </p>
                          <p className="mt-1 text-[11px] text-stone-400 dark:text-stone-500">
                            {intl.formatMessage({ id: 'memory.history.validFrom' })}{' '}
                            <Mono className="text-[11px] text-stone-500 dark:text-stone-400">
                              {atResult.record.valid_from
                                ? new Date(atResult.record.valid_from).toLocaleString()
                                : '—'}
                            </Mono>
                          </p>
                        </>
                      ) : (
                        <p className="text-xs text-stone-400 dark:text-stone-500">
                          {intl.formatMessage({ id: 'memory.history.pit.none' })}
                        </p>
                      )}
                    </div>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

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

function EvolutionTab() {
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
    // Aggregate error surfacing — one toast covers the pair so a double
    // backend outage doesn't stack two notifications on the user.
    let notified = false;
    const onFailure = (e: unknown) => {
      console.warn("[api]", e);
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

  return (
    <div className="space-y-4">
      {/* Mode banner */}
      {mode && (
        <div className={cn(
          'flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border px-4 py-3',
          enabled
            ? 'border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20'
            : 'border-[var(--panel-border)] bg-stone-500/5 dark:bg-white/5',
        )}>
          <div className="flex items-center gap-2">
            <GitBranch className={cn(
              'h-4 w-4',
              enabled ? 'text-amber-600 dark:text-amber-400' : 'text-stone-400',
            )} />
            <span className={cn(
              'text-sm',
              enabled ? 'text-amber-700 dark:text-amber-400' : 'text-stone-500 dark:text-stone-400',
            )}>
              {intl.formatMessage({ id: 'evolution.mode' })}: <strong>{mode.replace('_', ' ')}</strong>
            </span>
          </div>
          <span className="text-xs text-stone-500 dark:text-stone-400">
            {gvuEnabledCount}/{agents.length} {intl.formatMessage({ id: 'evolution.agentsEnabled' })}
          </span>
          {totalVersions > 0 && (
            <span className="text-xs text-stone-500 dark:text-stone-400">
              · {totalVersions} versions
            </span>
          )}
          {lastAppliedAt && (
            <span className="flex items-center gap-1 text-xs text-stone-500 dark:text-stone-400">
              <Clock className="h-3 w-3" />
              <Mono className="text-stone-500 dark:text-stone-400">{new Date(lastAppliedAt).toLocaleString()}</Mono>
            </span>
          )}
        </div>
      )}

      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : agents.length === 0 ? (
        <Card>
          <EmptyState
            icon={GitBranch}
            dudu="idle"
            title={intl.formatMessage({ id: 'common.noData' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <Card key={agent.agent_id}>
              <div className="mb-4 flex items-center gap-3">
                <CharacterAvatar agentId={agent.agent_id} name={agent.agent_id} size={28} />
                <h3 className="font-semibold text-stone-900 dark:text-stone-50">{agent.agent_id}</h3>
              </div>

              <div className="space-y-2 text-sm">
                <EvolutionRow
                  label="GVU"
                  enabled={agent.gvu_enabled}
                />
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

              <div className="mt-4 grid grid-cols-3 gap-2 border-t border-[var(--panel-border)] pt-4">
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.max_gvu_generations}
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxGenerations' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.observation_period_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.observationPeriod' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.max_silence_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxSilence' })}</p>
                </div>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* SOUL.md evolution history */}
      {!loading && agents.length > 0 && (
        <Section
          title={
            <span className="flex items-center gap-2">
              <GitBranch className="h-4 w-4 text-amber-600 dark:text-amber-400" />
              {intl.formatMessage({ id: 'evolution.engine' })}
            </span>
          }
        >
          {versions.length === 0 ? (
            <Card>
              <EmptyState
                icon={GitBranch}
                dudu="reading"
                title={intl.formatMessage({ id: 'evolution.noHistory' })}
              />
            </Card>
          ) : (
            <div className="space-y-2">
              {versions.map((v) => (
                <EvolutionVersionCard key={v.version_id} version={v} />
              ))}
            </div>
          )}
        </Section>
      )}
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
  const statusTone: Record<string, 'success' | 'danger' | 'warning'> = {
    Confirmed: 'success',
    RolledBack: 'danger',
    Observing: 'warning',
  };

  const renderDelta = (pre: number, post: number | undefined, invert = false) => {
    if (post === undefined || post === null) return (
      <span className="text-stone-400">{pre.toFixed(2)}</span>
    );
    const delta = post - pre;
    const good = invert ? delta < 0 : delta > 0;
    const color = Math.abs(delta) < 1e-6
      ? 'text-stone-500'
      : good ? 'text-emerald-600 dark:text-emerald-400' : 'text-rose-600 dark:text-rose-400';
    return (
      <span>
        <span className="text-stone-500">{pre.toFixed(2)}</span>
        <span className="mx-1 text-stone-400">→</span>
        <span className={color}>{post.toFixed(2)}</span>
      </span>
    );
  };

  return (
    <Card bodyClassName="p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <CharacterAvatar agentId={version.agent_id} name={version.agent_id} size={24} />
          <span className="text-sm font-medium text-amber-600 dark:text-amber-400">
            {version.agent_id}
          </span>
          <Badge tone={statusTone[version.status] ?? 'neutral'}>
            {statusLabel}
          </Badge>
          <Mono className="text-[10px] text-stone-400">{version.soul_hash.slice(0, 8)}</Mono>
        </div>
        <span className="flex items-center gap-1 text-xs text-stone-400">
          <Clock className="h-3 w-3" />
          <Mono className="text-stone-400">{new Date(version.applied_at).toLocaleString()}</Mono>
        </span>
      </div>
      {version.soul_summary && (
        <p className="mt-2 text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
          {version.soul_summary}
        </p>
      )}
      <div className="mt-3 grid grid-cols-3 gap-2 border-t border-[var(--panel-border)] pt-3 text-xs">
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.feedback' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.positive_feedback_ratio, version.post_metrics?.positive_feedback_ratio)}
          </p>
        </div>
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.error' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.prediction_error, version.post_metrics?.prediction_error, true)}
          </p>
        </div>
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.corrections' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.user_correction_rate, version.post_metrics?.user_correction_rate, true)}
          </p>
        </div>
      </div>
    </Card>
  );
}

function EvolutionRow({ label, enabled }: { label: string; enabled: boolean }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      {enabled ? (
        <CheckCircle className="h-4 w-4 text-emerald-500" />
      ) : (
        <XCircle className="h-4 w-4 text-stone-300 dark:text-stone-600" />
      )}
    </div>
  );
}

function InsightsTab() {
  const intl = useIntl();
  const [facts, setFacts] = useState<ReadonlyArray<KeyFactEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  // Load agent list on mount (mirrors MemoriesTab to keep UX consistent).
  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0 && !selectedAgent) {
        setSelectedAgent(list[0].name);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; see MemoriesTab for rationale.
  }, []);

  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    api.memory.keyFacts(selectedAgent, 50).then((res) => {
      setFacts(res?.entries ?? []);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setFacts([]);
    }).finally(() => setLoading(false));
  }, [selectedAgent, intl]);

  return (
    <div className="space-y-4">
      {/* Agent selector */}
      <Toolbar>
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className={cn(controlClass, 'w-auto')}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
      </Toolbar>

      {/* Insight cards */}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : facts.length === 0 ? (
        <Card>
          <EmptyState
            icon={Lightbulb}
            dudu="idle"
            title={intl.formatMessage({ id: 'memory.empty.insights' })}
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {facts.map((fact) => (
            <Card key={fact.id}>
              <div className="mb-2 flex items-start justify-between gap-3">
                <div className="flex items-center gap-2">
                  <CharacterAvatar agentId={fact.agent_id} name={fact.agent_id} size={24} />
                  <Lightbulb className="h-4 w-4 text-amber-500 dark:text-amber-400" />
                  <span className="text-xs font-medium text-amber-600 dark:text-amber-400">
                    {fact.agent_id}
                  </span>
                  {fact.access_count > 0 && (
                    <Badge tone="accent">
                      {intl.formatMessage(
                        { id: 'memory.insights.accessCount' },
                        { count: fact.access_count },
                      )}
                    </Badge>
                  )}
                </div>
                <span className="flex shrink-0 items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
                  <Clock className="h-3 w-3" />
                  <Mono className="text-stone-400 dark:text-stone-500">{new Date(fact.timestamp).toLocaleString()}</Mono>
                </span>
              </div>
              <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
                {fact.fact}
              </p>
              {(fact.source_session || fact.channel || fact.chat_id) && (
                <div className="mt-3 flex flex-wrap gap-x-3 gap-y-1 border-t border-[var(--panel-border)] pt-2 text-[11px] text-stone-400 dark:text-stone-500">
                  {fact.source_session && (
                    <span>session: <Mono className="text-[11px] text-stone-500 dark:text-stone-400">{fact.source_session}</Mono></span>
                  )}
                  {fact.channel && (
                    <span>channel: <Mono className="text-[11px] text-stone-500 dark:text-stone-400">{fact.channel}</Mono></span>
                  )}
                  {fact.chat_id && (
                    <span>chat: <Mono className="text-[11px] text-stone-500 dark:text-stone-400">{fact.chat_id}</Mono></span>
                  )}
                </div>
              )}
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
