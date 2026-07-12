import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type ReliabilitySummary, type EvolutionEvent } from '@/lib/api';
import {
  Activity,
  ShieldCheck,
  CheckCircle2,
  Puzzle,
  GitFork,
  RefreshCw,
  AlertTriangle,
  Sparkles,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  Button,
  Toolbar,
  Mono,
  CharacterAvatar,
  EmptyState,
  controlClass,
} from '@/components/ui';

// ── Window options ────────────────────────────────────────────────────────────

type WindowDays = 7 | 14 | 30;

// ── Evolution events (self-evolution audit trail) ───────────────────────────────

function evolutionOutcomeTone(outcome: string): 'success' | 'warning' | 'danger' | 'neutral' {
  const o = outcome.toLowerCase();
  if (o.includes('success') || o.includes('accept') || o.includes('adopt') || o.includes('confirm')) return 'success';
  if (o.includes('fail') || o.includes('reject') || o.includes('rollback') || o.includes('error')) return 'danger';
  if (o.includes('pending') || o.includes('observ') || o.includes('extend') || o.includes('trigger')) return 'warning';
  return 'neutral';
}

function EvolutionEventsSection({
  agentId,
  windowDays,
}: {
  agentId: string;
  windowDays: WindowDays;
}) {
  const intl = useIntl();
  const [events, setEvents] = useState<EvolutionEvent[] | null>(null);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!agentId) return;
    setLoading(true);
    setError(null);
    const since = new Date(Date.now() - windowDays * 24 * 60 * 60 * 1000).toISOString();
    try {
      const res = await api.audit.evolutionQuery({ agent_id: agentId, since, limit: 25 });
      setEvents(res.events ?? []);
      setTotal(res.total ?? 0);
    } catch (e) {
      // A missing/empty index must not blank the whole page — degrade to an
      // inline error and keep the reliability gauges above intact.
      console.warn('[evolution]', e);
      setError(formatError(e));
      setEvents([]);
    } finally {
      setLoading(false);
    }
  }, [agentId, windowDays]);

  useEffect(() => { load(); }, [load]);

  return (
    <Card
      bodyClassName="space-y-3"
      title={
        <span className="flex items-center gap-2">
          <Sparkles className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'evolution.events.title' })}
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">
        {intl.formatMessage({ id: 'evolution.events.desc' })}
      </p>

      {loading ? (
        <div className="space-y-2">
          {[0, 1, 2].map((i) => (
            <div key={i} className="h-9 animate-pulse rounded-lg bg-stone-500/5 dark:bg-white/5" />
          ))}
        </div>
      ) : error ? (
        <div className="flex items-center gap-2 rounded-lg border border-rose-200 bg-rose-50 px-3 py-2.5 text-sm text-rose-700 dark:border-rose-800/50 dark:bg-rose-900/20 dark:text-rose-400">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <span>{intl.formatMessage({ id: 'evolution.events.error' }, { message: error })}</span>
        </div>
      ) : events && events.length > 0 ? (
        <>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[36rem] text-left text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)] text-xs uppercase text-stone-500 dark:text-stone-400">
                  <th className="py-2 pr-3 font-medium">{intl.formatMessage({ id: 'evolution.events.col.type' })}</th>
                  <th className="py-2 pr-3 font-medium">{intl.formatMessage({ id: 'evolution.events.col.outcome' })}</th>
                  <th className="py-2 pr-3 font-medium">{intl.formatMessage({ id: 'evolution.events.col.detail' })}</th>
                  <th className="py-2 pl-3 text-right font-medium">{intl.formatMessage({ id: 'evolution.events.col.time' })}</th>
                </tr>
              </thead>
              <tbody>
                {events.map((ev, i) => (
                  <tr key={i} className="border-b border-[var(--panel-border)]/50 last:border-0">
                    <td className="py-2 pr-3">
                      <span className="font-medium text-stone-700 dark:text-stone-300">{ev.event_type}</span>
                    </td>
                    <td className="py-2 pr-3">
                      <Badge tone={evolutionOutcomeTone(ev.outcome)}>{ev.outcome}</Badge>
                    </td>
                    <td className="py-2 pr-3 text-stone-500 dark:text-stone-400">
                      {ev.skill_id ? <Mono className="text-xs">{ev.skill_id}</Mono> : ev.trigger_signal || '—'}
                    </td>
                    <td className="py-2 pl-3 text-right">
                      <Mono className="text-xs text-stone-400 dark:text-stone-500">
                        {new Date(ev.timestamp).toLocaleString('zh-TW', {
                          month: 'short',
                          day: 'numeric',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </Mono>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {total > events.length && (
            <p className="text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'evolution.events.more' }, { shown: events.length, total })}
            </p>
          )}
        </>
      ) : (
        <EmptyState
          icon={Sparkles}
          title={intl.formatMessage({ id: 'evolution.events.empty' })}
          hint={intl.formatMessage({ id: 'evolution.events.empty.desc' })}
        />
      )}
    </Card>
  );
}

// ── Gauge component ───────────────────────────────────────────────────────────

function MetricGauge({
  label,
  description,
  value,
  icon: Icon,
  invertBad,
}: {
  label: string;
  description: string;
  value: number;
  icon: React.ComponentType<{ className?: string }>;
  /** If true, a HIGH value is BAD (e.g. fallback_trigger_rate). Default false. */
  invertBad?: boolean;
}) {
  const pct = Math.round(value * 100);

  // Color thresholds — inverted for metrics where high = bad
  const isGood = invertBad ? value <= 0.1 : value >= 0.85;
  const isOk = invertBad ? value <= 0.25 : value >= 0.6;

  const barColor = isGood
    ? 'bg-emerald-500'
    : isOk
      ? 'bg-amber-500'
      : 'bg-rose-500';

  const textColor = isGood
    ? 'text-emerald-600 dark:text-emerald-400'
    : isOk
      ? 'text-amber-600 dark:text-amber-400'
      : 'text-rose-600 dark:text-rose-400';

  const iconBg = isGood
    ? 'bg-emerald-100 dark:bg-emerald-900/30'
    : isOk
      ? 'bg-amber-100 dark:bg-amber-900/30'
      : 'bg-rose-100 dark:bg-rose-900/30';

  return (
    <Card>
      <div className="mb-3 flex items-start gap-3">
        <div className={cn('rounded-lg p-2', iconBg)}>
          <Icon className={cn('h-5 w-5', textColor)} />
        </div>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-stone-700 dark:text-stone-300">{label}</p>
          <p className="text-xs text-stone-400 dark:text-stone-500">{description}</p>
        </div>
        <span className={cn('text-2xl font-bold tabular-nums', textColor)}>
          {pct}%
        </span>
      </div>

      {/* Progress bar */}
      <div className="h-2.5 w-full overflow-hidden rounded-full bg-stone-100 dark:bg-stone-800">
        <div
          className={cn('h-full rounded-full transition-all duration-700', barColor)}
          style={{ width: `${pct}%` }}
        />
      </div>
    </Card>
  );
}

// ── Empty / loading skeleton ──────────────────────────────────────────────────

function SkeletonGauge() {
  return (
    <Card>
      <div className="mb-3 flex items-start gap-3">
        <div className="h-9 w-9 animate-pulse rounded-lg bg-stone-200 dark:bg-stone-700" />
        <div className="flex-1 space-y-1.5">
          <div className="h-4 w-32 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
          <div className="h-3 w-48 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
        </div>
        <div className="h-8 w-12 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
      </div>
      <div className="h-2.5 w-full animate-pulse rounded-full bg-stone-200 dark:bg-stone-700" />
    </Card>
  );
}

// ── Main page ─────────────────────────────────────────────────────────────────

export function ReliabilityPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents } = useAgentsStore();

  const [selectedAgent, setSelectedAgent] = useState<string>('');
  const [windowDays, setWindowDays] = useState<WindowDays>(7);
  const [summary, setSummary] = useState<ReliabilitySummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  // Fetch agents on connection
  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchAgents();
  }, [connectionState, fetchAgents]);

  // Auto-select first agent
  useEffect(() => {
    if (agents.length > 0 && !selectedAgent) {
      setSelectedAgent(agents[0].name);
    }
  }, [agents, selectedAgent]);

  // Fetch reliability summary
  const fetchSummary = useCallback(
    async (agentId: string, days: WindowDays, silent = false) => {
      if (!agentId) return;
      if (!silent) setLoading(true);
      else setRefreshing(true);

      try {
        const result = await api.audit.reliabilitySummary(agentId, days);
        setSummary(result);
      } catch (e) {
        console.warn('[reliability]', e);
        toast.error(
          intl.formatMessage(
            { id: 'toast.error.loadFailed' },
            { message: formatError(e) },
          ),
        );
      } finally {
        setLoading(false);
        setRefreshing(false);
      }
    },
    [intl],
  );

  // Re-fetch when agent or window changes
  useEffect(() => {
    if (connectionState !== 'authenticated' || !selectedAgent) return;
    fetchSummary(selectedAgent, windowDays);
  }, [connectionState, selectedAgent, windowDays, fetchSummary]);

  const handleRefresh = () => {
    if (selectedAgent) fetchSummary(selectedAgent, windowDays, true);
  };

  const noData = summary !== null && summary.total_events === 0;

  return (
    <Page>
      <PageHeader
        icon={Activity}
        title={intl.formatMessage({ id: 'nav.reliability' })}
        subtitle={intl.formatMessage({ id: 'reliability.title' })}
        actions={
          <Toolbar>
            {/* Agent selector */}
            <select
              value={selectedAgent}
              onChange={(e) => setSelectedAgent(e.target.value)}
              className={cn(controlClass, 'w-auto')}
            >
              {agents.map((a) => (
                <option key={a.name} value={a.name}>
                  {a.name}
                </option>
              ))}
            </select>

            {/* Window selector */}
            <div className="flex gap-1 rounded-control border border-[var(--panel-border)] bg-[var(--panel-fill)] p-1">
              {([7, 14, 30] as WindowDays[]).map((d) => (
                <button
                  key={d}
                  onClick={() => setWindowDays(d)}
                  className={cn(
                    'rounded-md px-3 py-1 text-sm font-medium transition-colors',
                    windowDays === d
                      ? 'bg-amber-500 text-white shadow-sm'
                      : 'text-stone-600 hover:text-stone-900 dark:text-stone-400 dark:hover:text-stone-200',
                  )}
                >
                  {intl.formatMessage({ id: 'reliability.window.days' }, { count: d })}
                </button>
              ))}
            </div>

            {/* Refresh */}
            <Button
              variant="secondary"
              size="md"
              icon={RefreshCw}
              onClick={handleRefresh}
              disabled={refreshing || loading}
              title={intl.formatMessage({ id: 'common.refresh' })}
              className={cn(refreshing && '[&_svg]:animate-spin')}
            />
          </Toolbar>
        }
      />

      {/* Metadata row */}
      {summary && !loading && (
        <div className="flex flex-wrap items-center gap-4 rounded-control bg-stone-500/5 px-4 py-2.5 text-sm dark:bg-white/5">
          <span className="flex items-center gap-2 text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reliability.agent' })}
            {': '}
            <CharacterAvatar agentId={summary.agent_id} name={summary.agent_id} size={24} />
            <span className="font-medium text-stone-700 dark:text-stone-300">
              {summary.agent_id}
            </span>
          </span>
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reliability.events' })}
            {': '}
            <Mono className="font-medium text-stone-700 dark:text-stone-300">
              {summary.total_events.toLocaleString()}
            </Mono>
          </span>
          <span className="ml-auto flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'reliability.generatedAt' })}
            {': '}
            <Mono className="text-xs text-stone-400 dark:text-stone-500">
              {new Date(summary.generated_at).toLocaleString('zh-TW', {
                month: 'short',
                day: 'numeric',
                hour: '2-digit',
                minute: '2-digit',
              })}
            </Mono>
          </span>
        </div>
      )}

      {/* No audit data banner */}
      {noData && !loading && (
        <div className="flex items-center gap-3 rounded-control border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-700 dark:border-amber-800/50 dark:bg-amber-900/20 dark:text-amber-400">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          {intl.formatMessage({ id: 'reliability.noEvents' }, { days: windowDays })}
        </div>
      )}

      {/* Metric gauges */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        {loading ? (
          <>
            <SkeletonGauge />
            <SkeletonGauge />
            <SkeletonGauge />
            <SkeletonGauge />
          </>
        ) : summary ? (
          <>
            <MetricGauge
              label={intl.formatMessage({ id: 'reliability.metric.consistencyScore' })}
              description={intl.formatMessage({ id: 'reliability.metric.consistencyScore.desc' })}
              value={summary.consistency_score}
              icon={ShieldCheck}
            />
            <MetricGauge
              label={intl.formatMessage({ id: 'reliability.metric.taskSuccessRate' })}
              description={intl.formatMessage({ id: 'reliability.metric.taskSuccessRate.desc' })}
              value={summary.task_success_rate}
              icon={CheckCircle2}
            />
            <MetricGauge
              label={intl.formatMessage({ id: 'reliability.metric.skillAdoptionRate' })}
              description={intl.formatMessage({ id: 'reliability.metric.skillAdoptionRate.desc' })}
              value={summary.skill_adoption_rate}
              icon={Puzzle}
            />
            <MetricGauge
              label={intl.formatMessage({ id: 'reliability.metric.fallbackTriggerRate' })}
              description={intl.formatMessage({ id: 'reliability.metric.fallbackTriggerRate.desc' })}
              value={summary.fallback_trigger_rate}
              icon={GitFork}
              invertBad
            />
          </>
        ) : null}
      </div>

      {/* Legend */}
      {!loading && (
        <Card>
          <div className="flex flex-wrap items-center gap-6">
            <p className="text-sm font-medium text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'reliability.legend.title' })}
            </p>
            <div className="flex flex-wrap gap-3">
              <Badge tone="success" dot>
                {intl.formatMessage({ id: 'reliability.legend.good' })}
              </Badge>
              <Badge tone="warning" dot>
                {intl.formatMessage({ id: 'reliability.legend.ok' })}
              </Badge>
              <Badge tone="danger" dot>
                {intl.formatMessage({ id: 'reliability.legend.poor' })}
              </Badge>
            </div>
            <p className="ml-auto text-xs text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'reliability.legend.note' })}
            </p>
          </div>
        </Card>
      )}

      {/* Self-evolution audit trail for the selected agent + window. */}
      {selectedAgent && (
        <EvolutionEventsSection agentId={selectedAgent} windowDays={windowDays} />
      )}
    </Page>
  );
}
