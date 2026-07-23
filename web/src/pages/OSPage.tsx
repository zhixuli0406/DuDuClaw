import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { client } from '@/lib/ws-client';
import {
  api,
  OS_NATIVE_QUOTA_ERROR_CODE,
  type OsAgentStatus,
  type OsDoctorCheck,
  type OsDoctorStatus,
  type OsEventPush,
  type OsEventRow,
  type OsGateDecision,
  type OsGateQuadrants,
  type OsGateRecentResult,
  type OsGateRow,
  type OsSettingsUpdateParams,
  type OsStatusResult,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  MonitorCog,
  RefreshCw,
  Radar,
  ListChecks,
  Stethoscope,
  CircleCheck,
  CircleX,
  CircleSlash,
  TriangleAlert,
} from 'lucide-react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
  Empty,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Skeleton,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  TooltipProvider,
  type BadgeProps,
} from '@/components/mds';
import { OsAgentCard } from '@/components/os/OsAgentCard';
import { OsQuadrantChart } from '@/components/os/OsQuadrantChart';

/**
 * OSPage — dashboard "OS" page (P4-3 frontend). Reports + settings surface
 * for the OS-native perception stack (filesystem watch / frontmost polling /
 * digital footprint / proactive gate). Layout mixes the Settings and
 * Report/Usage paradigms (DESIGN.md §3.3 / §3.4): a settings-style card grid
 * for per-agent toggles, and report-style panels for the proactivity gate,
 * recent events, and the on-demand environment doctor.
 *
 * All five RPCs (os.status / os.settings.update / os.gate.recent /
 * os.events.recent / os.doctor.run) are admin-gated server-side; the nav
 * entry mirrors that with `minRole: 'admin'` (UX-only — see nav-visibility.ts).
 */

// ── Small structured-error helpers (RPC rejects with `{ code, message }`) ──

function errorCode(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'code' in err) {
    const c = (err as { code?: unknown }).code;
    return typeof c === 'string' ? c : undefined;
  }
  return undefined;
}

function errorMessage(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'message' in err) {
    const m = (err as { message?: unknown }).message;
    return typeof m === 'string' ? m : undefined;
  }
  return undefined;
}

// ── Local report-panel shell (mirrors ReportPage's `ReportCard`) ──────────

function Panel({
  icon: Icon,
  title,
  description,
  action,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
        <div className="min-w-0 space-y-1">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <Icon className="size-4 text-muted-foreground" />
            {title}
          </CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </div>
        {action}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

// ── Proactivity gate decision tone ─────────────────────────────────────────

function decisionTone(decision: OsGateDecision): { variant: NonNullable<BadgeProps['variant']>; className?: string } {
  return decision === 'allow'
    ? { variant: 'secondary', className: 'bg-success/15 text-success' }
    : { variant: 'outline' };
}

// Ring-buffer cap for the live perception-event tail (P4-3+) — keeps the
// page's memory bounded no matter how chatty an agent's `[os_watch]` is.
const MAX_LIVE_EVENTS = 200;

const QUADRANT_KEYS: ReadonlyArray<keyof OsGateQuadrants> = [
  'correct_detection',
  'false_alarm',
  'missed_need',
  'correct_silence',
  'non_response',
  'unknown',
];

// ── Doctor check status → icon/tone ────────────────────────────────────────

const DOCTOR_STATUS_ICON: Record<OsDoctorStatus, React.ComponentType<{ className?: string }>> = {
  ok: CircleCheck,
  warn: TriangleAlert,
  fail: CircleX,
  skip: CircleSlash,
};

const DOCTOR_STATUS_CLASS: Record<OsDoctorStatus, string> = {
  ok: 'text-success',
  warn: 'text-warning',
  fail: 'text-destructive',
  skip: 'text-muted-foreground',
};

function DoctorRow({ check }: { check: OsDoctorCheck }) {
  const intl = useIntl();
  const Icon = DOCTOR_STATUS_ICON[check.status];
  return (
    <div className="flex items-start gap-3 rounded-lg border border-surface-border px-3 py-2.5">
      <Icon className={cn('mt-0.5 size-4 shrink-0', DOCTOR_STATUS_CLASS[check.status])} />
      <div className="min-w-0 space-y-0.5">
        <p className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: `os.doctor.check.${check.id}` })}
        </p>
        <p className="text-xs text-muted-foreground">{check.detail}</p>
      </div>
    </div>
  );
}

// ── Main page ───────────────────────────────────────────────────────────────

export function OSPage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const connectionState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents } = useAgentsStore();

  // ── Overview: fleet status ──
  const [status, setStatus] = useState<OsStatusResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [savingField, setSavingField] = useState<string | null>(null);

  const fetchStatus = useCallback(
    async (silent = false) => {
      if (!silent) setLoading(true);
      else setRefreshing(true);
      try {
        const res = await api.os.status();
        setStatus(res);
      } catch (e) {
        console.warn('[os]', e);
        toast.error(t('toast.error.loadFailed', { message: formatError(e) }));
      } finally {
        setLoading(false);
        setRefreshing(false);
      }
    },
    [t],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchStatus();
    fetchAgents();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionState]);

  const displayNameOf = useMemo(() => {
    const map = new Map(agents.map((a) => [a.name, a.display_name || a.name]));
    return (agentId: string) => map.get(agentId) ?? agentId;
  }, [agents]);

  // Every switch/number edit goes through `os.settings.update` then a full
  // refetch — the authoritative source for fields an optimistic patch can't
  // safely derive client-side (quota.used, live watch/frontmost stats).
  const handleAgentUpdate = useCallback(
    async (agentId: string, patch: Omit<OsSettingsUpdateParams, 'agent_id'>, fieldKey: string) => {
      const key = `${agentId}:${fieldKey}`;
      setSavingField(key);
      try {
        const res = await api.os.settingsUpdate({ agent_id: agentId, ...patch });
        toast.success(
          t(res.hot_reloaded || res.os_watch_hot_reloaded ? 'os.toast.saved.live' : 'os.toast.saved.deferred'),
        );
        await fetchStatus(true);
      } catch (e) {
        console.warn('[os]', e);
        if (errorCode(e) === OS_NATIVE_QUOTA_ERROR_CODE) {
          toast.error(errorMessage(e) ?? t('os.quota.toast'));
        } else {
          toast.error(t('toast.error.saveFailed', { message: formatError(e) }));
        }
      } finally {
        setSavingField((k) => (k === key ? null : k));
      }
    },
    [t, fetchStatus],
  );

  // ── Proactivity gate report ──
  const [gateAgentFilter, setGateAgentFilter] = useState('');
  const [gateData, setGateData] = useState<OsGateRecentResult | null>(null);
  const [gateLoading, setGateLoading] = useState(true);

  const fetchGate = useCallback(
    async (agentId: string, silent = false) => {
      if (!silent) setGateLoading(true);
      try {
        const res = await api.os.gateRecent({ n: 50, ...(agentId ? { agent_id: agentId } : {}) });
        setGateData(res);
      } catch (e) {
        console.warn('[os.gate]', e);
        setGateData(null);
        toast.error(t('toast.error.loadFailed', { message: formatError(e) }));
      } finally {
        setGateLoading(false);
      }
    },
    [t],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchGate(gateAgentFilter);
  }, [connectionState, gateAgentFilter, fetchGate]);

  const quadrantLabels = useMemo(
    () =>
      QUADRANT_KEYS.reduce(
        (acc, k) => {
          acc[k] = t(`os.gate.quadrant.${k}`);
          return acc;
        },
        {} as Record<keyof OsGateQuadrants, string>,
      ),
    [t],
  );

  // ── Recent perception events: on-demand snapshot + live WS tail (P4-3+) ──
  // Initial load is always the `os.events.recent` snapshot. Once authenticated,
  // the page also opts into the live `os.events.entry` tail (admin-gated RPC,
  // same as every other `os.*` call) and prepends pushed rows in front of it.
  // A ring buffer caps the list so a very active watcher can't grow this
  // page's memory unbounded. Connection loss degrades to snapshot-only — the
  // manual refresh button keeps working either way.
  const [events, setEvents] = useState<OsEventRow[]>([]);
  const [eventsLoading, setEventsLoading] = useState(true);
  const [liveActive, setLiveActive] = useState(false);
  // Synthetic ids for live-pushed rows (negative, decrementing — real DB ids
  // from `os.events.recent` are always positive) double as the "just arrived"
  // marker for the entrance animation: once a row mounts with this class the
  // browser doesn't replay it on later re-renders, so no timer/cleanup needed.
  const nextSyntheticIdRef = useRef(-1);

  const fetchEvents = useCallback(async () => {
    setEventsLoading(true);
    try {
      const res = await api.os.eventsRecent({ n: 50 });
      setEvents(res.events ?? []);
    } catch (e) {
      console.warn('[os.events]', e);
      setEvents([]);
      toast.error(t('toast.error.loadFailed', { message: formatError(e) }));
    } finally {
      setEventsLoading(false);
    }
  }, [t]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchEvents();
  }, [connectionState, fetchEvents]);

  useEffect(() => {
    if (connectionState !== 'authenticated') {
      setLiveActive(false);
      return;
    }
    let cancelled = false;
    api.os
      .eventsSubscribe()
      .then(() => {
        if (!cancelled) setLiveActive(true);
      })
      .catch((e) => {
        console.warn('[os.events.subscribe]', e);
        if (!cancelled) setLiveActive(false);
      });
    const unsubscribe = client.subscribe('os.events.entry', (payload) => {
      const push = payload as OsEventPush;
      const row: OsEventRow = { ...push, id: nextSyntheticIdRef.current-- };
      setEvents((prev) => [row, ...prev].slice(0, MAX_LIVE_EVENTS));
    });
    return () => {
      cancelled = true;
      setLiveActive(false);
      unsubscribe();
      api.os.eventsUnsubscribe().catch((e) => console.warn('[os.events.unsubscribe]', e));
    };
  }, [connectionState]);

  // ── Doctor (on-demand, expensive — never auto-run) ──
  const [doctorChecks, setDoctorChecks] = useState<OsDoctorCheck[] | null>(null);
  const [doctorLoading, setDoctorLoading] = useState(false);

  const runDoctor = useCallback(async () => {
    setDoctorLoading(true);
    try {
      const res = await api.os.doctorRun();
      setDoctorChecks(res.checks ?? []);
    } catch (e) {
      console.warn('[os.doctor]', e);
      toast.error(t('toast.error.actionFailed', { message: formatError(e) }));
    } finally {
      setDoctorLoading(false);
    }
  }, [t]);

  const formatTs = (iso: string) =>
    new Date(iso).toLocaleString('zh-TW', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });

  return (
    <TooltipProvider delay={500}>
      <div className="mx-auto w-full max-w-6xl space-y-6">
        {/* Header */}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2">
            <MonitorCog className="size-5 text-muted-foreground" />
            <div>
              <h1 className="text-base font-medium">{t('nav.os')}</h1>
              <p className="text-sm text-muted-foreground">{t('os.subtitle')}</p>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {status && (
              <>
                <Badge variant="outline">{t(`edition.${status.edition}`)}</Badge>
                <Badge
                  variant="secondary"
                  className={
                    status.quota.limit !== null && status.quota.used >= status.quota.limit
                      ? 'bg-warning/15 text-warning'
                      : undefined
                  }
                >
                  {status.quota.limit === null
                    ? t('os.quota.unlimited', { used: status.quota.used })
                    : t('os.quota.limited', { used: status.quota.used, limit: status.quota.limit })}
                </Badge>
              </>
            )}
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => fetchStatus(true)}
              disabled={refreshing || loading}
              aria-label={t('common.refresh')}
              title={t('common.refresh')}
            >
              <RefreshCw className={cn(refreshing && 'animate-spin')} />
            </Button>
          </div>
        </div>

        {/* Overview: per-agent OS-native cards */}
        <section className="space-y-3">
          <h2 className="text-sm font-medium text-foreground">{t('os.section.overview')}</h2>
          {loading ? (
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
              <Skeleton className="h-56 w-full rounded-xl" />
              <Skeleton className="h-56 w-full rounded-xl" />
            </div>
          ) : !status || status.agents.length === 0 ? (
            <Empty icon={MonitorCog} title={t('os.empty.title')} description={t('os.empty.desc')} />
          ) : (
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
              {status.agents.map((agent: OsAgentStatus) => {
                const wouldExceedQuota =
                  !agent.os_native && status.quota.limit !== null && status.quota.used >= status.quota.limit;
                return (
                  <OsAgentCard
                    key={agent.agent_id}
                    agent={agent}
                    displayName={displayNameOf(agent.agent_id)}
                    quotaBlocked={wouldExceedQuota ? t('os.quota.tooltip') : null}
                    savingField={savingField}
                    onUpdate={(patch, fieldKey) => handleAgentUpdate(agent.agent_id, patch, fieldKey)}
                  />
                );
              })}
            </div>
          )}
        </section>

        {/* Proactivity report */}
        <Panel
          icon={Radar}
          title={t('os.section.gate')}
          description={t('os.section.gate.desc')}
          action={
            status && status.agents.length > 0 ? (
              <Select value={gateAgentFilter} onValueChange={(v) => setGateAgentFilter(String(v))}>
                <SelectTrigger size="sm" className="w-40">
                  <SelectValue>
                    {gateAgentFilter ? displayNameOf(gateAgentFilter) : t('os.gate.filter.allAgents')}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="">{t('os.gate.filter.allAgents')}</SelectItem>
                  {status.agents.map((a) => (
                    <SelectItem key={a.agent_id} value={a.agent_id}>
                      {displayNameOf(a.agent_id)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : undefined
          }
        >
          {gateLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-40 w-full" />
              <Skeleton className="h-24 w-full" />
            </div>
          ) : !gateData || gateData.recent.length === 0 ? (
            <Empty icon={Radar} title={t('os.gate.empty.title')} description={t('os.gate.empty.desc')} />
          ) : (
            <div className="space-y-5">
              <OsQuadrantChart
                quadrants={gateData.quadrants}
                labels={quadrantLabels}
                titleForAria={t('os.section.gate')}
                className="max-w-xl"
              />
              <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t('os.gate.col.time')}</TableHead>
                      <TableHead>{t('os.gate.col.agent')}</TableHead>
                      <TableHead>{t('os.gate.col.event')}</TableHead>
                      <TableHead className="text-right">{t('os.gate.col.score')}</TableHead>
                      <TableHead>{t('os.gate.col.decision')}</TableHead>
                      <TableHead>{t('os.gate.col.reason')}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {gateData.recent.map((row: OsGateRow, i) => {
                      const tone = decisionTone(row.decision);
                      return (
                        <TableRow key={`${row.ts}-${i}`}>
                          <TableCell className="font-mono text-xs whitespace-nowrap text-muted-foreground">
                            {formatTs(row.ts)}
                          </TableCell>
                          <TableCell className="text-foreground">{displayNameOf(row.agent)}</TableCell>
                          <TableCell>
                            <span className="font-mono text-xs text-muted-foreground">{row.event}</span>
                          </TableCell>
                          <TableCell className="text-right font-mono text-xs tabular-nums text-muted-foreground">
                            {row.score} / {row.threshold}
                          </TableCell>
                          <TableCell>
                            <Badge variant={tone.variant} className={tone.className}>
                              {t(`os.gate.decision.${row.decision}`)}
                            </Badge>
                          </TableCell>
                          <TableCell className="max-w-xs truncate text-muted-foreground" title={row.reason}>
                            {row.reason || '—'}
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}
        </Panel>

        {/* Recent perception events */}
        <Panel
          icon={ListChecks}
          title={t('os.section.events')}
          description={
            liveActive
              ? t('os.section.events.desc.live')
              : connectionState === 'authenticated'
                ? t('os.section.events.desc')
                : t('os.events.live.disconnected')
          }
          action={
            <div className="flex items-center gap-2">
              <span
                className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
                title={liveActive ? t('os.events.live.tooltip') : t('os.events.live.disconnected')}
              >
                <span
                  className={cn(
                    'size-2 shrink-0 rounded-full',
                    liveActive ? 'bg-success animate-pulse' : 'bg-muted-foreground/40',
                  )}
                />
                {liveActive ? t('os.events.live') : t('status.disconnected')}
              </span>
              <Button
                variant="ghost"
                size="icon-sm"
                onClick={fetchEvents}
                disabled={eventsLoading}
                aria-label={t('common.refresh')}
                title={t('common.refresh')}
              >
                <RefreshCw className={cn(eventsLoading && 'animate-spin')} />
              </Button>
            </div>
          }
        >
          {eventsLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
            </div>
          ) : events.length === 0 ? (
            <Empty icon={ListChecks} title={t('os.events.empty.title')} description={t('os.events.empty.desc')} />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t('os.events.col.time')}</TableHead>
                    <TableHead>{t('os.events.col.event')}</TableHead>
                    <TableHead>{t('os.events.col.source')}</TableHead>
                    <TableHead>{t('os.events.col.payload')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {events.map((ev) => {
                    const payloadText = (() => {
                      try {
                        return JSON.stringify(ev.payload);
                      } catch {
                        return '';
                      }
                    })();
                    // Synthetic ids (from the live tail) are negative — real
                    // `os.events.recent` DB ids are always positive. Reusing
                    // that sign as the "just arrived via push" marker avoids a
                    // separate fresh-row tracking Set; the animation plays
                    // once on mount and never replays on later re-renders of
                    // the same DOM node (reduced-motion gated in index.css).
                    const isFresh = ev.id < 0;
                    return (
                      <TableRow key={ev.id} className={cn(isFresh && 'animate-fade-up')}>
                        <TableCell className="font-mono text-xs whitespace-nowrap text-muted-foreground">
                          {formatTs(ev.ts)}
                        </TableCell>
                        <TableCell>
                          <span className="font-mono text-xs text-foreground">{ev.event}</span>
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">{ev.source ?? '—'}</TableCell>
                        <TableCell
                          className="max-w-sm truncate font-mono text-xs text-muted-foreground"
                          title={payloadText}
                        >
                          {payloadText || '—'}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </div>
          )}
        </Panel>

        {/* Environment doctor (on-demand, expensive) */}
        <Panel
          icon={Stethoscope}
          title={t('os.section.doctor')}
          description={t('os.section.doctor.desc')}
          action={
            <Button variant="outline" size="sm" onClick={runDoctor} disabled={doctorLoading}>
              <Stethoscope className={cn(doctorLoading && 'animate-pulse')} />
              {t('os.doctor.run')}
            </Button>
          }
        >
          {doctorChecks === null ? (
            <Empty icon={Stethoscope} title={t('os.doctor.empty.title')} description={t('os.doctor.empty.desc')} />
          ) : (
            <div className="space-y-2">
              {doctorChecks.map((check) => (
                <DoctorRow key={check.id} check={check} />
              ))}
            </div>
          )}
        </Panel>
      </div>
    </TooltipProvider>
  );
}
