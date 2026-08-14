import { useState, useEffect, useMemo, useCallback } from 'react';
import type { IntlShape } from 'react-intl';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import {
  api,
  type EvolutionVersion,
  type EvolutionStagnationSignal,
  type EvolutionStagnationSnapshot,
  type EvolutionTelemetrySummary,
  type EvolutionConsolidation,
  type PlaybookEntry,
  type RuleStatusKey,
  type KeyFactEntry,
} from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { withParam } from '@/lib/url-params';
import { toast } from '@/lib/toast';
import { useSystemStore } from '@/stores/system-store';
import { MemoryBrowser } from '@/components/memory/MemoryBrowser';
import { KnowledgeHubPage } from './KnowledgeHubPage';
import { SharedWikiPage } from './SharedWikiPage';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  useErrorMessage,
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  CardContent,
  Segmented,
  Badge,
  Button,
  Input,
  Skeleton,
  ActorAvatar,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  type SegmentedOption,
} from '@/components/mds';
import {
  BrainIcon,
  SearchIcon,
  ClockIcon,
  GitBranchIcon,
  CheckCircleIcon,
  XCircleIcon,
  LightbulbIcon,
  AlertTriangleIcon,
  BookOpenIcon,
  DownloadIcon,
  Trash2Icon,
} from 'lucide-react';

/**
 * MemoryPage — one "記憶" surface covering everything the AI staff member
 * remembers (2026-07-30 client feedback: "can the knowledge base and memory be
 * merged into 記憶?"). The Segmented switcher spans five views:
 *
 *   記憶       — auto-accumulated memory, grouped by topic (MemoryBrowser)
 *   個人知識庫  — the agent's own curated wiki (KnowledgeHubPage, embedded)
 *   共享知識庫  — the cross-agent wiki (SharedWikiPage, embedded) — shown on
 *                both editions; on Personal it surfaces what all AI staff
 *                share
 *   觀察洞察    — extracted key facts
 *   自主學習    — SOUL.md evolution status and version history
 *
 * The active view mirrors to `?tab=` so the legacy `/knowledge` route can
 * redirect straight into the knowledge tab and deep links keep working.
 */

type ViewId = 'memories' | 'wiki' | 'shared' | 'insights' | 'evolution';

const VIEW_IDS: readonly ViewId[] = ['memories', 'wiki', 'shared', 'insights', 'evolution'];

function parseView(raw: string | null): ViewId {
  const v = VIEW_IDS.find((id) => id === raw);
  return v ?? 'memories';
}

export function MemoryPage() {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [params, setParams] = useSearchParams();
  const view = parseView(params.get('tab'));
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');
  // W3-3 (state-as-URL): the memories search term starts from `?q=` so a
  // bookmarked/shared search opens with the same query already typed.
  const [query, setQuery] = useState(() => params.get('q') ?? '');

  const setView = (id: ViewId) => {
    const next = new URLSearchParams(params);
    next.set('tab', id);
    setParams(next, { replace: true });
  };

  // Mirror the search term back into the URL (W3-3). Functional update so it
  // never clobbers `tab` (or MemoryBrowser's own `cat` param below).
  useEffect(() => {
    setParams((prev) => withParam(prev, 'q', query), { replace: true });
  }, [query, setParams]);

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) setSelectedAgent((prev) => prev || list[0].name);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
    });
    // Run once on mount; intl is stable from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const viewOptions: SegmentedOption<ViewId>[] = useMemo(() => {
    const opts: SegmentedOption<ViewId>[] = [
      { value: 'memories', label: intl.formatMessage({ id: 'memory.tab.memories' }) },
      {
        value: 'wiki',
        // On the Personal edition there is only one knowledge base, so it is
        // labelled plainly rather than "個人 / 共享".
        label: intl.formatMessage({
          id: isPersonal ? 'memory.tab.knowledge' : 'memory.tab.knowledge.personal',
        }),
      },
    ];
    opts.push({
      value: 'shared',
      label: intl.formatMessage({ id: 'memory.tab.knowledge.shared' }),
    });
    opts.push(
      { value: 'insights', label: intl.formatMessage({ id: 'memory.tab.insights' }) },
      { value: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }) },
    );
    return opts;
  }, [intl, isPersonal]);

  // The wiki views bring their own agent picker; the shared wiki is not
  // agent-scoped at all. Evolution's overview cards are all-agent, but the
  // stagnation/telemetry/consolidations/playbook detail sections underneath
  // need one agent selected — same picker, reused.
  const showAgentPicker =
    view === 'memories' || view === 'insights' || view === 'evolution';

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
        {showAgentPicker && (
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
        {view === 'memories' && <MemoryBrowser agentId={selectedAgent} query={query} />}
        {view === 'wiki' && <KnowledgeHubPage embedded />}
        {view === 'shared' && <SharedWikiPage embedded />}
        {view === 'insights' && <InsightsView agentId={selectedAgent} />}
        {view === 'evolution' && <EvolutionView selectedAgent={selectedAgent} />}
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

function MemoryListSkeleton() {
  return (
    <div className="flex flex-col gap-1.5" role="status" aria-label="Loading">
      {Array.from({ length: 6 }).map((_, i) => (
        <Skeleton key={i} className="h-9 w-full" />
      ))}
    </div>
  );
}

// ── Insights view (key facts) ───────────────────────────────

function InsightsView({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [facts, setFacts] = useState<ReadonlyArray<KeyFactEntry>>([]);
  const [loading, setLoading] = useState(false);
  // A failed read used to land on the same empty state as "nothing learned
  // yet", with only a toast to tell them apart (P05, phase-4 audit).
  const [loadError, setLoadError] = useState<unknown>(null);
  const [reloadNonce, setReloadNonce] = useState(0);

  useEffect(() => {
    if (!agentId) return;
    setLoading(true);
    api.memory.keyFacts(agentId, 50).then((res) => {
      setFacts(res?.entries ?? []);
      setLoadError(null);
    }).catch((e: unknown) => {
      console.warn('[api]', e);
      setFacts([]);
      setLoadError(e);
    }).finally(() => setLoading(false));
  }, [agentId, reloadNonce]);

  if (loading) return <MemoryListSkeleton />;

  if (loadError != null) {
    return (
      <ErrorState
        icon={LightbulbIcon}
        error={loadError}
        onRetry={() => setReloadNonce((n) => n + 1)}
      />
    );
  }

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
  // WP5b / D7 — the backend still reports `cognitive_memory`, but it is
  // always-on infrastructure now, so the page no longer renders it as a
  // per-agent capability row.
  skill_auto_activate: boolean;
  skill_security_scan: boolean;
  max_silence_hours: number;
  max_gvu_generations: number;
  observation_period_hours: number;
}

function EvolutionView({ selectedAgent }: { selectedAgent: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
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
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
      return null;
    };
    Promise.all([
      api.evolution.status().catch(onFailure),
      // Superset of `.history` (adds the WP0.4 ExpiredNoData status + the
      // low-data alert flag); same optional-agent/limit contract.
      api.evolution.versions(undefined, 20).catch(onFailure),
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

      {/* Per-agent detail: stagnation watch + rejection distribution. Silent
          when the agent has no signal to show (§ "Real-time without anxiety"
          — a quiet dashboard is the healthy state, not a missing feature). */}
      {selectedAgent && (
        <div className="grid gap-4 lg:grid-cols-2">
          <StagnationCard agentId={selectedAgent} />
          <TelemetryCard agentId={selectedAgent} />
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

      {selectedAgent && (
        <>
          <ConsolidationsCard agentId={selectedAgent} />
          <PlaybookCard agentId={selectedAgent} />
        </>
      )}
    </div>
  );
}

// ── Stagnation watch (AVO §2.4) ──────────────────────────────

function stagnationSignalLabel(intl: IntlShape, s: EvolutionStagnationSignal): string {
  switch (s.kind) {
    case 'consecutive_non_applied':
      return intl.formatMessage(
        { id: 'evolution.stagnation.signal.consecutive' },
        { count: s.count ?? 0, threshold: s.threshold ?? 0 },
      );
    case 'zero_apply_window':
      return intl.formatMessage(
        { id: 'evolution.stagnation.signal.zeroApply' },
        { days: s.days ?? 0, count: s.trigger_count ?? 0 },
      );
    case 'repeated_rejection_reason':
      return intl.formatMessage(
        { id: 'evolution.stagnation.signal.repeated' },
        { occurrences: s.occurrences ?? 0, threshold: s.threshold ?? 0 },
      );
    default:
      return s.kind;
  }
}

function StagnationCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [snapshot, setSnapshot] = useState<EvolutionStagnationSnapshot | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    api.evolution.stagnation(agentId).then((res) => {
      if (alive) setSnapshot(res?.snapshots?.[0] ?? null);
    }).catch((e) => {
      console.warn('[api]', e);
      if (alive) setSnapshot(null);
    }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [agentId]);

  if (loading) return <Skeleton className="h-24 w-full" />;

  const stagnant = snapshot?.is_stagnant ?? false;

  return (
    <Card data-size="sm" className={cn(stagnant && 'border-warning/40 bg-warning/5')}>
      <CardContent className="space-y-2">
        <div className="flex items-center gap-2">
          {stagnant ? (
            <AlertTriangleIcon className="size-4 shrink-0 text-warning" />
          ) : (
            <CheckCircleIcon className="size-4 shrink-0 text-success" />
          )}
          <h3 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'evolution.stagnation.title' })}
          </h3>
        </div>
        {stagnant ? (
          <ul className="space-y-1 pl-6 text-sm text-muted-foreground">
            {(snapshot?.signals ?? []).map((s, i) => (
              <li key={i} className="list-disc">{stagnationSignalLabel(intl, s)}</li>
            ))}
          </ul>
        ) : (
          <p className="pl-6 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'evolution.stagnation.ok' })}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

// ── Rejection distribution (WP0.6) ───────────────────────────

function TelemetryCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [days, setDays] = useState<'7' | '30'>('7');
  const [summary, setSummary] = useState<EvolutionTelemetrySummary | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    api.evolution.telemetry(agentId, Number(days)).then((res) => {
      if (alive) setSummary(res ?? null);
    }).catch((e) => {
      console.warn('[api]', e);
      if (alive) setSummary(null);
    }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [agentId, days]);

  const rows = useMemo(() => {
    if (!summary) return [];
    const out: Array<{ stage: string; layer: string; count: number }> = [];
    for (const [stage, layers] of Object.entries(summary.by_stage_layer)) {
      for (const [layer, count] of Object.entries(layers)) {
        out.push({ stage, layer, count });
      }
    }
    return out.sort((a, b) => b.count - a.count).slice(0, 8);
  }, [summary]);
  const max = Math.max(1, ...rows.map((r) => r.count));

  const rangeOptions: SegmentedOption<'7' | '30'>[] = [
    { value: '7', label: intl.formatMessage({ id: 'evolution.telemetry.range.7d' }) },
    { value: '30', label: intl.formatMessage({ id: 'evolution.telemetry.range.30d' }) },
  ];

  return (
    <Card data-size="sm">
      <CardHeader>
        <CardTitle className="text-sm">{intl.formatMessage({ id: 'evolution.telemetry.title' })}</CardTitle>
        <CardAction>
          <Segmented value={days} onValueChange={setDays} options={rangeOptions} />
        </CardAction>
      </CardHeader>
      <CardContent>
        {loading ? (
          <Skeleton className="h-20 w-full" />
        ) : rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'evolution.telemetry.empty' })}
          </p>
        ) : (
          <div className="space-y-2">
            {rows.map((r) => (
              <div key={`${r.stage}-${r.layer}`} className="flex items-center gap-2 text-xs">
                <span className="w-36 shrink-0 truncate text-muted-foreground">
                  {intl.formatMessage({ id: `evolution.telemetry.stage.${r.stage}`, defaultMessage: r.stage })}
                  {' · '}
                  <span className="font-mono">{r.layer}</span>
                </span>
                <div className="h-2 min-w-0 flex-1 overflow-hidden rounded-full bg-muted">
                  <div
                    className="h-full rounded-full bg-chart-1"
                    style={{ width: `${(r.count / max) * 100}%` }}
                  />
                </div>
                <span className="w-8 shrink-0 text-right font-mono tabular-nums text-foreground">{r.count}</span>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ── Consolidation audit trail (WP0.2) ────────────────────────

function ConsolidationsCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [records, setRecords] = useState<ReadonlyArray<EvolutionConsolidation>>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    api.evolution.consolidations(agentId, 10).then((res) => {
      if (alive) setRecords(res?.consolidations ?? []);
    }).catch((e) => {
      console.warn('[api]', e);
      if (alive) setRecords([]);
    }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [agentId]);

  // Quiet when there is nothing to audit — no consolidation has ever been
  // needed for this agent, which is the common (and healthy) case.
  if (!loading && records.length === 0) return null;

  const outcomeBadgeClass = (outcome: string) => {
    if (outcome === 'applied') return 'bg-success/15 text-success';
    if (outcome === 'attempted') return 'bg-info/15 text-info';
    return 'bg-destructive/10 text-destructive';
  };

  return (
    <section className="space-y-2">
      <h2 className="text-sm font-medium text-foreground">
        {intl.formatMessage({ id: 'evolution.consolidations.title' })}
      </h2>
      {loading ? (
        <Skeleton className="h-16 w-full" />
      ) : (
        <div className="space-y-1.5">
          {records.map((r) => (
            <Card key={r.id} data-size="sm">
              <CardContent className="flex flex-wrap items-center justify-between gap-2">
                <Badge variant="secondary" className={outcomeBadgeClass(r.outcome)}>
                  {intl.formatMessage({
                    id: `evolution.consolidations.outcome.${r.outcome}`,
                    defaultMessage: r.outcome,
                  })}
                </Badge>
                <span className="font-mono text-xs tabular-nums text-muted-foreground">
                  {r.from_bytes}B → {r.to_bytes ?? '—'}B
                </span>
                <span className="flex items-center gap-1 font-mono text-xs tabular-nums text-muted-foreground">
                  <ClockIcon className="size-3" />
                  {timeAgo(r.attempted_at)}
                </span>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </section>
  );
}

// ── Playbook (WP1.2/1.3 gene-shaped experience entries) ──────

const PLAYBOOK_CATEGORY_CLASS: Record<string, string> = {
  repair: 'bg-destructive/10 text-destructive',
  optimize: 'bg-info/15 text-info',
  innovate: 'bg-brand/12 text-brand',
  regulatory: 'bg-muted text-muted-foreground',
  explore: 'bg-warning/15 text-warning',
};

const PLAYBOOK_STATE_CLASS: Record<string, string> = {
  probation: 'bg-warning/15 text-warning',
  active: 'bg-success/15 text-success',
  stale: 'bg-muted text-muted-foreground',
  retired: 'bg-muted text-muted-foreground/70',
};

/**
 * W3-2 (§C.9) — status badge colours keyed by the *outward* vocabulary the
 * gateway derives, not by the raw stored state. `observing` and `trial` are
 * separate colours on purpose: 「觀察中(尚未生效)」 means the rule is not
 * steering anything yet, which reads very differently from 「試用中」.
 */
const RULE_STATUS_CLASS: Record<RuleStatusKey, string> = {
  observing: 'bg-info/15 text-info',
  trial: 'bg-warning/15 text-warning',
  active: 'bg-success/15 text-success',
  dormant: 'bg-muted text-muted-foreground',
  retired: 'bg-muted text-muted-foreground/70',
};

function PlaybookEntryRow({ entry, onRetire }: { entry: PlaybookEntry; onRetire: () => void }) {
  const intl = useIntl();
  const retired = entry.state === 'retired';
  const h = entry.humanized;

  return (
    <Card data-size="sm">
      <CardContent className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant="secondary" className={PLAYBOOK_CATEGORY_CLASS[entry.category]}>
              {intl.formatMessage({ id: `playbook.category.${entry.category}` })}
            </Badge>
            {h ? (
              <Badge variant="secondary" className={RULE_STATUS_CLASS[h.status_key]}>
                {intl.formatMessage({ id: `playbook.status.${h.status_key}` })}
              </Badge>
            ) : (
              <Badge variant="secondary" className={PLAYBOOK_STATE_CLASS[entry.state]}>
                {intl.formatMessage({ id: `playbook.state.${entry.state}` })}
              </Badge>
            )}
          </div>
          {!retired && (
            <Button variant="ghost" size="icon-xs" onClick={onRetire} aria-label={intl.formatMessage({ id: 'playbook.retire.button' })} title={intl.formatMessage({ id: 'playbook.retire.button' })}>
              <Trash2Icon />
            </Button>
          )}
        </div>

        {/* Plain language leads; the model-facing text is one click away. An
            older gateway sends no `humanized`, so the raw content stays the
            primary text there rather than the card rendering empty. */}
        {h && !h.fallback ? (
          <p className="whitespace-pre-wrap text-sm text-foreground">{h.sentence}</p>
        ) : (
          <>
            {h?.fallback && (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'playbook.humanize.fallback' })}
              </p>
            )}
            <p className="whitespace-pre-wrap text-sm text-foreground">{entry.content}</p>
          </>
        )}

        {h && (
          <p className="text-xs text-muted-foreground">
            <span className="font-medium text-foreground/80">
              {intl.formatMessage({ id: 'playbook.why.label' })}
            </span>
            {' '}
            {h.why}
          </p>
        )}

        {h && !h.fallback && (
          <details className="group">
            <summary className="cursor-pointer list-none rounded text-xs text-muted-foreground underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden">
              {intl.formatMessage({ id: 'playbook.raw.toggle' })}
            </summary>
            <p className="mt-1.5 whitespace-pre-wrap rounded-md bg-muted/40 p-2 font-mono text-xs text-muted-foreground">
              {entry.content}
            </p>
          </details>
        )}

        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-surface-border pt-2 text-xs text-muted-foreground">
          <span>
            {intl.formatMessage({ id: 'playbook.stats' }, { helpful: entry.helpful, harmful: entry.harmful })}
          </span>
          {entry.success_streak > 0 && (
            <span>{intl.formatMessage({ id: 'playbook.streak' }, { count: entry.success_streak })}</span>
          )}
          {entry.eval_cases.length > 0 && (
            <span>{intl.formatMessage({ id: 'playbook.evalCases' }, { count: entry.eval_cases.length })}</span>
          )}
          {(h?.evidence.failure_notes ?? 0) > 0 && (
            <span>
              {intl.formatMessage(
                { id: 'playbook.failureNotes' },
                { count: h?.evidence.failure_notes ?? 0 },
              )}
            </span>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function PlaybookCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const [entries, setEntries] = useState<ReadonlyArray<PlaybookEntry>>([]);
  const [loading, setLoading] = useState(true);
  const [retireTarget, setRetireTarget] = useState<PlaybookEntry | null>(null);
  const [reason, setReason] = useState('');
  const [retiring, setRetiring] = useState(false);

  const load = useCallback(() => {
    setLoading(true);
    api.playbook.list(agentId).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
      setEntries([]);
    }).finally(() => setLoading(false));
    // Run once per agent; `intl` is stable from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentId]);

  useEffect(() => { load(); }, [load]);

  const handleExport = async () => {
    try {
      const res = await api.playbook.export(agentId);
      const blob = new Blob([JSON.stringify(res, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${agentId}-playbook.json`;
      a.click();
      URL.revokeObjectURL(url);
      toast.success(intl.formatMessage({ id: 'playbook.export.success' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'playbook.export.error' }, { message: errorText(e) }));
    }
  };

  const handleRetireConfirm = async () => {
    if (!retireTarget) return;
    setRetiring(true);
    try {
      const res = await api.playbook.retire(agentId, retireTarget.id, reason);
      if (!res.retired) {
        toast.error(intl.formatMessage({ id: 'playbook.retire.error' }, { message: res.reason ?? '' }));
      } else {
        toast.success(intl.formatMessage({ id: 'playbook.retire.success' }));
      }
      setRetireTarget(null);
      setReason('');
      load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'playbook.retire.error' }, { message: errorText(e) }));
    } finally {
      setRetiring(false);
    }
  };

  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'playbook.title' })}
        </h2>
        <Button variant="outline" size="xs" onClick={handleExport} disabled={loading || entries.length === 0}>
          <DownloadIcon />
          {intl.formatMessage({ id: 'playbook.export.button' })}
        </Button>
      </div>

      {loading ? (
        <Skeleton className="h-24 w-full" />
      ) : entries.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={BookOpenIcon}
          title={intl.formatMessage({ id: 'playbook.empty' })}
        />
      ) : (
        <div className="space-y-1.5">
          {entries.map((entry) => (
            <PlaybookEntryRow key={entry.id} entry={entry} onRetire={() => setRetireTarget(entry)} />
          ))}
        </div>
      )}

      <Dialog
        open={retireTarget !== null}
        onOpenChange={(o) => { if (!o) { setRetireTarget(null); setReason(''); } }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'playbook.retire.confirm.title' })}</DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'playbook.retire.confirm.desc' })}
            </p>
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'playbook.retire.confirm.reasonLabel' })}
              </label>
              <Input value={reason} onChange={(e) => setReason(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => { setRetireTarget(null); setReason(''); }} disabled={retiring}>
              {intl.formatMessage({ id: 'playbook.retire.confirm.cancel' })}
            </Button>
            <Button variant="destructive" onClick={handleRetireConfirm} disabled={retiring}>
              {retiring
                ? intl.formatMessage({ id: 'common.saving' })
                : intl.formatMessage({ id: 'playbook.retire.confirm.confirm' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
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
      // WP0.4: the observation window closed without enough traffic to judge
      // pass/fail — deliberately NOT phrased as pass or fail (user-facing
      // copy, never the internal `ExpiredNoData` term).
      case 'ExpiredNoData': return intl.formatMessage({ id: 'evolution.status.expiredNoData' });
      default: return version.status;
    }
  })();
  const statusClass: Record<string, string> = {
    Confirmed: 'bg-success/15 text-success',
    RolledBack: 'bg-destructive/10 text-destructive',
    Observing: 'bg-warning/15 text-warning',
    ExpiredNoData: 'bg-muted text-muted-foreground',
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
        {version.status === 'ExpiredNoData' && version.low_data_alert_sent && (
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'evolution.version.lowDataAlert' })}
          </p>
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
