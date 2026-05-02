import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type ReliabilitySummary } from '@/lib/api';
import {
  ShieldCheck,
  CheckCircle2,
  Puzzle,
  GitFork,
  RefreshCw,
  AlertTriangle,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';

// ── Window options ────────────────────────────────────────────────────────────

type WindowDays = 7 | 14 | 30;

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
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
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
    </div>
  );
}

// ── Empty / loading skeleton ──────────────────────────────────────────────────

function SkeletonGauge() {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
      <div className="mb-3 flex items-start gap-3">
        <div className="h-9 w-9 animate-pulse rounded-lg bg-stone-200 dark:bg-stone-700" />
        <div className="flex-1 space-y-1.5">
          <div className="h-4 w-32 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
          <div className="h-3 w-48 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
        </div>
        <div className="h-8 w-12 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
      </div>
      <div className="h-2.5 w-full animate-pulse rounded-full bg-stone-200 dark:bg-stone-700" />
    </div>
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
    <div className="space-y-6">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'reliability.title' })}
        </h2>

        <div className="flex flex-wrap items-center gap-2">
          {/* Agent selector */}
          <select
            value={selectedAgent}
            onChange={(e) => setSelectedAgent(e.target.value)}
            className="rounded-lg border border-stone-200 bg-white px-3 py-1.5 text-sm text-stone-700 focus:outline-none focus:ring-2 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-900 dark:text-stone-300"
          >
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.name}
              </option>
            ))}
          </select>

          {/* Window selector */}
          <div className="flex gap-1 rounded-lg border border-stone-200 bg-stone-100 p-1 dark:border-stone-700 dark:bg-stone-800">
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
          <button
            onClick={handleRefresh}
            disabled={refreshing || loading}
            className="rounded-lg border border-stone-200 bg-white p-1.5 text-stone-500 transition-colors hover:bg-stone-50 hover:text-stone-700 disabled:opacity-50 dark:border-stone-700 dark:bg-stone-900 dark:text-stone-400 dark:hover:bg-stone-800"
            title={intl.formatMessage({ id: 'common.refresh' })}
          >
            <RefreshCw
              className={cn('h-4 w-4', refreshing && 'animate-spin')}
            />
          </button>
        </div>
      </div>

      {/* Metadata row */}
      {summary && !loading && (
        <div className="flex flex-wrap items-center gap-4 rounded-lg bg-stone-50 px-4 py-2.5 text-sm dark:bg-stone-800/50">
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reliability.agent' })}
            {': '}
            <span className="font-medium text-stone-700 dark:text-stone-300">
              {summary.agent_id}
            </span>
          </span>
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reliability.events' })}
            {': '}
            <span className="font-medium text-stone-700 dark:text-stone-300">
              {summary.total_events.toLocaleString()}
            </span>
          </span>
          <span className="ml-auto text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'reliability.generatedAt' })}
            {': '}
            {new Date(summary.generated_at).toLocaleString('zh-TW', {
              month: 'short',
              day: 'numeric',
              hour: '2-digit',
              minute: '2-digit',
            })}
          </span>
        </div>
      )}

      {/* No audit data banner */}
      {noData && !loading && (
        <div className="flex items-center gap-3 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-700 dark:border-amber-800/50 dark:bg-amber-900/20 dark:text-amber-400">
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
        <div className="flex flex-wrap items-center gap-6 rounded-xl border border-stone-200 bg-white px-5 py-4 dark:border-stone-800 dark:bg-stone-900">
          <p className="text-sm font-medium text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reliability.legend.title' })}
          </p>
          <div className="flex flex-wrap gap-4 text-xs text-stone-500 dark:text-stone-400">
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2.5 w-2.5 rounded-full bg-emerald-500" />
              {intl.formatMessage({ id: 'reliability.legend.good' })}
            </span>
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2.5 w-2.5 rounded-full bg-amber-500" />
              {intl.formatMessage({ id: 'reliability.legend.ok' })}
            </span>
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2.5 w-2.5 rounded-full bg-rose-500" />
              {intl.formatMessage({ id: 'reliability.legend.poor' })}
            </span>
          </div>
          <p className="ml-auto text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'reliability.legend.note' })}
          </p>
        </div>
      )}
    </div>
  );
}
