import { useState, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import { api, type EvolutionVersion, type KeyFactEntry } from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import { useSystemStore } from '@/stores/system-store';
import { MemoryBrowser } from '@/components/memory/MemoryBrowser';
import { KnowledgeHubPage } from './KnowledgeHubPage';
import { SharedWikiPage } from './SharedWikiPage';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Segmented,
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
  LightbulbIcon,
} from 'lucide-react';

/**
 * MemoryPage — one "記憶" surface covering everything the AI staff member
 * remembers (2026-07-30 client feedback: "can the knowledge base and memory be
 * merged into 記憶?"). The Segmented switcher spans five views:
 *
 *   記憶       — auto-accumulated memory, grouped by topic (MemoryBrowser)
 *   個人知識庫  — the agent's own curated wiki (KnowledgeHubPage, embedded)
 *   共享知識庫  — the cross-agent wiki (SharedWikiPage, embedded) — enterprise
 *                only; on the Personal edition there is only one knowledge
 *                base, so the tab is hidden entirely
 *   觀察洞察    — extracted key facts
 *   自主學習    — SOUL.md evolution status and version history
 *
 * The active view mirrors to `?tab=` so the legacy `/knowledge` route can
 * redirect straight into the knowledge tab and deep links keep working.
 */

type ViewId = 'memories' | 'wiki' | 'shared' | 'insights' | 'evolution';

const VIEW_IDS: readonly ViewId[] = ['memories', 'wiki', 'shared', 'insights', 'evolution'];

function parseView(raw: string | null, allowShared: boolean): ViewId {
  const v = VIEW_IDS.find((id) => id === raw);
  if (!v) return 'memories';
  if (v === 'shared' && !allowShared) return 'wiki';
  return v;
}

export function MemoryPage() {
  const intl = useIntl();
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [params, setParams] = useSearchParams();
  const view = parseView(params.get('tab'), !isPersonal);
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');
  const [query, setQuery] = useState('');

  const setView = (id: ViewId) => {
    const next = new URLSearchParams(params);
    next.set('tab', id);
    setParams(next, { replace: true });
  };

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
    if (!isPersonal) {
      opts.push({
        value: 'shared',
        label: intl.formatMessage({ id: 'memory.tab.knowledge.shared' }),
      });
    }
    opts.push(
      { value: 'insights', label: intl.formatMessage({ id: 'memory.tab.insights' }) },
      { value: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }) },
    );
    return opts;
  }, [intl, isPersonal]);

  // The wiki views bring their own agent picker; evolution and the shared wiki
  // are not agent-scoped at all.
  const showAgentPicker = view === 'memories' || view === 'insights';

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
