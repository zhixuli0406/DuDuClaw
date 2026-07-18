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
  Button,
  Badge,
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Segmented,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  ActorAvatar,
  Empty,
  Skeleton,
  type SegmentedOption,
  type BadgeProps,
} from '@/components/mds';

// ── Window options ────────────────────────────────────────────────────────────

type WindowDays = 7 | 14 | 30;

// ── Evolution events (self-evolution audit trail) ───────────────────────────────

function evolutionOutcomeTone(outcome: string): { variant: NonNullable<BadgeProps['variant']>; className?: string } {
  const o = outcome.toLowerCase();
  if (o.includes('success') || o.includes('accept') || o.includes('adopt') || o.includes('confirm')) {
    return { variant: 'secondary', className: 'bg-success/15 text-success' };
  }
  if (o.includes('fail') || o.includes('reject') || o.includes('rollback') || o.includes('error')) {
    return { variant: 'destructive' };
  }
  if (o.includes('pending') || o.includes('observ') || o.includes('extend') || o.includes('trigger')) {
    return { variant: 'secondary', className: 'bg-warning/15 text-warning' };
  }
  return { variant: 'outline' };
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
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Sparkles className="size-4 text-brand" />
          {intl.formatMessage({ id: 'evolution.events.title' })}
        </CardTitle>
        <CardDescription>{intl.formatMessage({ id: 'evolution.events.desc' })}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {loading ? (
          <div className="space-y-2">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-9 w-full" />
            ))}
          </div>
        ) : error ? (
          <div className="flex items-center gap-2 rounded-lg bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
            <AlertTriangle className="size-4 shrink-0" />
            <span>{intl.formatMessage({ id: 'evolution.events.error' }, { message: error })}</span>
          </div>
        ) : events && events.length > 0 ? (
          <>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{intl.formatMessage({ id: 'evolution.events.col.type' })}</TableHead>
                  <TableHead>{intl.formatMessage({ id: 'evolution.events.col.outcome' })}</TableHead>
                  <TableHead>{intl.formatMessage({ id: 'evolution.events.col.detail' })}</TableHead>
                  <TableHead className="text-right">{intl.formatMessage({ id: 'evolution.events.col.time' })}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((ev, i) => {
                  const tone = evolutionOutcomeTone(ev.outcome);
                  return (
                    <TableRow key={i}>
                      <TableCell className="font-medium text-foreground">{ev.event_type}</TableCell>
                      <TableCell>
                        <Badge variant={tone.variant} className={tone.className}>{ev.outcome}</Badge>
                      </TableCell>
                      <TableCell className="text-muted-foreground">
                        {ev.skill_id ? <span className="font-mono text-xs">{ev.skill_id}</span> : ev.trigger_signal || '—'}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs text-muted-foreground">
                        {new Date(ev.timestamp).toLocaleString('zh-TW', {
                          month: 'short',
                          day: 'numeric',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
            {total > events.length && (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'evolution.events.more' }, { shown: events.length, total })}
              </p>
            )}
          </>
        ) : (
          <Empty
            icon={Sparkles}
            title={intl.formatMessage({ id: 'evolution.events.empty' })}
            description={intl.formatMessage({ id: 'evolution.events.empty.desc' })}
          />
        )}
      </CardContent>
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

  const barColor = isGood ? 'bg-success' : isOk ? 'bg-warning' : 'bg-destructive';
  const textColor = isGood ? 'text-success' : isOk ? 'text-warning' : 'text-destructive';
  const iconBg = isGood ? 'bg-success/15' : isOk ? 'bg-warning/15' : 'bg-destructive/15';

  return (
    <Card>
      <CardContent className="space-y-3">
        <div className="flex items-start gap-3">
          <div className={cn('rounded-lg p-2', iconBg)}>
            <Icon className={cn('size-5', textColor)} />
          </div>
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium text-foreground">{label}</p>
            <p className="text-xs text-muted-foreground">{description}</p>
          </div>
          <span className={cn('text-2xl font-bold tabular-nums', textColor)}>
            {pct}%
          </span>
        </div>

        {/* Progress bar */}
        <div className="h-2.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className={cn('h-full rounded-full transition-all duration-700', barColor)}
            style={{ width: `${pct}%` }}
          />
        </div>
      </CardContent>
    </Card>
  );
}

// ── Empty / loading skeleton ──────────────────────────────────────────────────

function SkeletonGauge() {
  return (
    <Card>
      <CardContent className="space-y-3">
        <div className="flex items-start gap-3">
          <Skeleton className="size-9 rounded-lg" />
          <div className="flex-1 space-y-1.5">
            <Skeleton className="h-4 w-32" />
            <Skeleton className="h-3 w-48" />
          </div>
          <Skeleton className="h-8 w-12" />
        </div>
        <Skeleton className="h-2.5 w-full rounded-full" />
      </CardContent>
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
  const currentAgent = agents.find((a) => a.name === selectedAgent);

  const windowOptions: SegmentedOption<string>[] = ([7, 14, 30] as WindowDays[]).map((d) => ({
    value: String(d),
    label: intl.formatMessage({ id: 'reliability.window.days' }, { count: d }),
  }));

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Activity className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.reliability' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'reliability.title' })}</p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {/* Agent selector */}
          {agents.length > 0 && (
            <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))}>
              <SelectTrigger className="w-40 sm:w-48">
                <SelectValue>
                  {currentAgent ? currentAgent.display_name || currentAgent.name : selectedAgent}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {agents.map((a) => (
                  <SelectItem key={a.name} value={a.name}>
                    {a.display_name || a.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}

          {/* Window selector */}
          <Segmented value={String(windowDays)} onValueChange={(v) => setWindowDays(Number(v) as WindowDays)} options={windowOptions} />

          {/* Refresh */}
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={handleRefresh}
            disabled={refreshing || loading}
            aria-label={intl.formatMessage({ id: 'common.refresh' })}
            title={intl.formatMessage({ id: 'common.refresh' })}
          >
            <RefreshCw className={cn(refreshing && 'animate-spin')} />
          </Button>
        </div>
      </div>

      {/* Metadata row */}
      {summary && !loading && (
        <div className="flex flex-wrap items-center gap-4 rounded-lg bg-muted px-4 py-2.5 text-sm">
          <span className="flex items-center gap-2 text-muted-foreground">
            {intl.formatMessage({ id: 'reliability.agent' })}
            {': '}
            <ActorAvatar actorType="agent" name={summary.agent_id} size="sm" />
            <span className="font-medium text-foreground">
              {summary.agent_id}
            </span>
          </span>
          <span className="text-muted-foreground">
            {intl.formatMessage({ id: 'reliability.events' })}
            {': '}
            <span className="font-mono font-medium text-foreground">
              {summary.total_events.toLocaleString()}
            </span>
          </span>
          <span className="ml-auto flex items-center gap-1 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'reliability.generatedAt' })}
            {': '}
            <span className="font-mono text-xs text-muted-foreground">
              {new Date(summary.generated_at).toLocaleString('zh-TW', {
                month: 'short',
                day: 'numeric',
                hour: '2-digit',
                minute: '2-digit',
              })}
            </span>
          </span>
        </div>
      )}

      {/* No audit data banner */}
      {noData && !loading && (
        <div className="flex items-center gap-3 rounded-lg bg-warning/10 px-4 py-3 text-sm text-warning">
          <AlertTriangle className="size-4 shrink-0" />
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
          <CardContent className="flex flex-wrap items-center gap-6">
            <p className="text-sm font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'reliability.legend.title' })}
            </p>
            <div className="flex flex-wrap gap-3">
              <Badge variant="secondary" className="bg-success/15 text-success">
                <span className="size-1.5 rounded-full bg-success" />
                {intl.formatMessage({ id: 'reliability.legend.good' })}
              </Badge>
              <Badge variant="secondary" className="bg-warning/15 text-warning">
                <span className="size-1.5 rounded-full bg-warning" />
                {intl.formatMessage({ id: 'reliability.legend.ok' })}
              </Badge>
              <Badge variant="destructive">
                <span className="size-1.5 rounded-full bg-destructive" />
                {intl.formatMessage({ id: 'reliability.legend.poor' })}
              </Badge>
            </div>
            <p className="ml-auto text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'reliability.legend.note' })}
            </p>
          </CardContent>
        </Card>
      )}

      {/* Self-evolution audit trail for the selected agent + window. */}
      {selectedAgent && (
        <EvolutionEventsSection agentId={selectedAgent} windowDays={windowDays} />
      )}
    </div>
  );
}
