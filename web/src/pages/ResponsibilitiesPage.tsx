import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  Card,
  CardContent,
  Badge,
  Button,
  Input,
  Textarea,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/mds';
import { CalendarSync, Eye, Pause, Play, Power, RotateCcw, Plus, AlertTriangle } from 'lucide-react';
import {
  api,
  type ResponsibilityRow,
  type ResponsibilitySummary,
  type ResponsibilityOccurrence,
  type ResponsibilityStatus,
  type ResponsibilityInput,
  type ResponsibilityCas,
} from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { StopTaskButton } from '@/components/task/StopTaskButton';

/**
 * ResponsibilitiesPage (`/responsibilities`, P5) — 持續任務.
 *
 * The P2-A backend made "an employee keeps an eye on something" a standing
 * contract; this page is its product surface. Everything shown is what the
 * gateway reports: when the feature is off the page says so and names the
 * two keys, it never draws sample rows. Every action is re-checked by the
 * server against the viewer's binding to the owner employee, so the buttons
 * hidden here are a convenience, not the gate.
 */

/** Parsed `scope_json` lane (`explore` = every run is read-only). */
export function laneOf(row: Pick<ResponsibilityRow, 'scope_json'>): 'explore' | 'normal' {
  try {
    const v = JSON.parse(row.scope_json) as { lane?: unknown };
    return v && v.lane === 'explore' ? 'explore' : 'normal';
  } catch {
    return 'normal';
  }
}

/** Which controls the server accepts for a responsibility in `state`. */
export function controlsFor(state: string): Array<'pause' | 'resume' | 'disable' | 'enable'> {
  switch (state) {
    case 'active':
      return ['pause', 'disable'];
    case 'paused':
    case 'budget_paused':
    case 'failure_paused':
      return ['resume', 'disable'];
    case 'disabled':
      return ['enable'];
    default:
      return [];
  }
}

export type PresetId = 'daily_briefing' | 'weekly_review' | 'custom';

export interface PresetForm {
  ownerAgentId: string;
  objective: string;
  acceptance: string;
  time: string;
  timezone: string;
  readOnly: boolean;
  runCapCents: number;
  periodCapCents: number;
  stopAfterDays: number;
}

/** Build the server input for a preset. Safe presets are read-only and
 *  run once per period with a small cap. */
export function buildInput(preset: PresetId, form: PresetForm, now = new Date()): ResponsibilityInput {
  const [hh, mm] = (form.time || '08:00').split(':').map((n) => Math.max(0, parseInt(n, 10) || 0));
  const weekly = preset === 'weekly_review';
  const cron = `0 ${Math.min(mm, 59)} ${Math.min(hh, 23)} * * ${weekly ? 'Mon' : '*'}`;
  const stopAt = new Date(now.getTime() + Math.max(1, form.stopAfterDays) * 86_400_000);
  const runCap = Math.max(1, Math.round(form.runCapCents));
  const input: ResponsibilityInput = {
    owner_agent_id: form.ownerAgentId,
    objective: form.objective.trim(),
    acceptance_template: form.acceptance.trim(),
    schedule: { cron, timezone: form.timezone },
    notification_policy: { enabled: false, on: [] },
    occurrence_hours: 1,
    occurrence_cost_cap_cents: runCap,
    budget_period: weekly ? 'week' : 'day',
    budget_timezone: form.timezone,
    period_cost_limit_cents: Math.max(runCap, Math.round(form.periodCapCents)),
    period_occurrence_limit: 1,
    min_wake_interval_secs: 3600,
    max_consecutive_failures: 3,
    stop_at: stopAt.toISOString(),
  };
  if (preset !== 'custom' || form.readOnly) input.lane = 'explore';
  return input;
}

const cents = (n: number | null | undefined) => (n == null ? '—' : `$${(n / 100).toFixed(2)}`);

function localTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}

function stateVariant(state: string): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (state === 'active') return 'default';
  if (state === 'failure_paused') return 'destructive';
  if (state === 'disabled' || state === 'expired') return 'outline';
  return 'secondary';
}

export function ResponsibilitiesPage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const connState = useConnectionStore((s) => s.state);
  const user = useAuthStore((s) => s.user);
  const bindings = useAuthStore((s) => s.bindings);

  const [status, setStatus] = useState<ResponsibilityStatus | null>(null);
  const [rows, setRows] = useState<ResponsibilityRow[] | null>(null);
  const [summaries, setSummaries] = useState<Record<string, ResponsibilitySummary>>({});
  const [loadError, setLoadError] = useState<unknown>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [occurrences, setOccurrences] = useState<ResponsibilityOccurrence[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const selectedRef = useRef<string | null>(null);

  const isAdmin = user?.role === 'admin';
  const isManager = isAdmin || user?.role === 'manager';
  const canOperate = useCallback(
    (agent: string) =>
      isAdmin ||
      bindings.some((b) => b.agent_name === agent && (b.access_level === 'operator' || b.access_level === 'owner')),
    [isAdmin, bindings],
  );
  const operableAgents = useMemo(
    () =>
      [...new Set(bindings.filter((b) => b.access_level !== 'viewer').map((b) => b.agent_name))].sort(),
    [bindings],
  );

  const load = useCallback(async () => {
    try {
      const st = await api.responsibilities.status();
      setStatus(st);
      const list = await api.responsibilities.list();
      const all = list.responsibilities ?? [];
      setRows(all);
      setLoadError(null);
      // One summary per row (next wake, this window's spend and runs).
      const pairs = await Promise.all(
        all.slice(0, 50).map(async (r) => {
          try {
            const s = await api.responsibilities.get(r.responsibility_id);
            return s.summary ? ([r.responsibility_id, s.summary] as const) : null;
          } catch {
            return null;
          }
        }),
      );
      setSummaries(Object.fromEntries(pairs.filter((p): p is NonNullable<typeof p> => p !== null)));
    } catch (e) {
      setLoadError(e);
    }
  }, []);

  useEffect(() => {
    if (connState === 'authenticated') void load();
  }, [connState, load]);

  const openDetail = useCallback(async (id: string) => {
    selectedRef.current = id;
    setSelected(id);
    setOccurrences(null);
    try {
      const r = await api.responsibilities.occurrences(id);
      if (selectedRef.current === id) setOccurrences((r.occurrences ?? []).slice(0, 20));
    } catch {
      if (selectedRef.current === id) setOccurrences([]);
    }
  }, []);

  const control = async (
    row: ResponsibilityRow,
    action: 'pause' | 'resume' | 'disable' | 'enable' | 'clear_failures',
  ) => {
    setBusy(row.responsibility_id);
    setNotice(null);
    try {
      const out: ResponsibilityCas = await api.responsibilities.control(
        action,
        row.responsibility_id,
        row.control_epoch,
        'dashboard',
      );
      if (out.conflict) setNotice(t('responsibilities.notice.conflict'));
      await load();
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const grouped = useMemo(() => {
    const m = new Map<string, ResponsibilityRow[]>();
    for (const r of rows ?? []) {
      const list = m.get(r.owner_agent_id) ?? [];
      list.push(r);
      m.set(r.owner_agent_id, list);
    }
    return [...m.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [rows]);

  const featureOff = status !== null && (!status.enabled || !status.dispatch_enabled);
  const selectedRow = rows?.find((r) => r.responsibility_id === selected) ?? null;
  const selectedSummary = selected ? summaries[selected] : undefined;

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={CalendarSync}
        title={t('responsibilities.title')}
        count={rows?.length}
        description={t('responsibilities.subtitle')}
        action={
          !featureOff && (isAdmin || operableAgents.length > 0) ? (
            <Button size="sm" onClick={() => setCreating(true)}>
              <Plus className="size-3.5" />
              {t('responsibilities.action.create')}
            </Button>
          ) : undefined
        }
      />

      <div className="flex-1 overflow-y-auto p-5">
        {featureOff && (
          <Card className="mb-4 border-amber-500/40" data-testid="responsibilities-off">
            <CardContent className="flex items-start gap-2 py-3 text-sm">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-500" />
              <div className="space-y-1">
                <p className="font-medium">{t('responsibilities.off.title')}</p>
                <p className="text-muted-foreground">
                  {t('responsibilities.off.desc', {
                    key: '[responsibilities] enabled = true',
                    dispatch: '[dispatch] enabled = true',
                  })}
                </p>
                {status && (
                  <p className="text-xs text-muted-foreground">
                    {t('responsibilities.off.state', {
                      resp: status.enabled ? 'on' : 'off',
                      dispatch: status.dispatch_enabled ? 'on' : 'off',
                    })}
                  </p>
                )}
              </div>
            </CardContent>
          </Card>
        )}
        {notice && (
          <Card className="mb-4 border-destructive/40">
            <CardContent className="py-3 text-sm text-destructive">{notice}</CardContent>
          </Card>
        )}

        {rows === null && loadError != null ? (
          <ErrorState icon={CalendarSync} error={loadError} onRetry={() => void load()} />
        ) : rows === null ? (
          <CollectionPageState state="loading" title={t('responsibilities.title')} />
        ) : rows.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={CalendarSync}
            title={t('responsibilities.empty.title')}
            description={t('responsibilities.empty.desc')}
          />
        ) : (
          <div className="space-y-6">
            {grouped.map(([agent, list]) => (
              <section key={agent} className="space-y-3">
                <h2 className="text-sm font-medium text-muted-foreground">{agent}</h2>
                {list.map((r) => {
                  const s = summaries[r.responsibility_id];
                  const lane = laneOf(r);
                  const operable = canOperate(r.owner_agent_id);
                  return (
                    <Card key={r.responsibility_id} data-testid="responsibility-row">
                      <CardContent className="flex flex-col gap-2 py-3">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-medium">{r.objective}</span>
                          <Badge variant={stateVariant(r.state)}>{t(`responsibilities.state.${r.state}`)}</Badge>
                          {lane === 'explore' && (
                            <Badge variant="outline" className="gap-1">
                              <Eye className="size-3" />
                              {t('responsibilities.lane.explore')}
                            </Badge>
                          )}
                        </div>
                        <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-muted-foreground sm:grid-cols-3">
                          <div>
                            <dt className="inline">{t('responsibilities.field.nextWake')}: </dt>
                            <dd className="inline">{s?.next_due_at ?? '—'}</dd>
                          </div>
                          <div>
                            <dt className="inline">{t('responsibilities.field.spend')}: </dt>
                            <dd className="inline">
                              {cents(s?.period_spent)} / {cents(r.period_cost_limit_cents)}
                            </dd>
                          </div>
                          <div>
                            <dt className="inline">{t('responsibilities.field.runs')}: </dt>
                            <dd className="inline">
                              {s ? s.period_occurrences : '—'} / {r.period_occurrence_limit}
                            </dd>
                          </div>
                          <div>
                            <dt className="inline">{t('responsibilities.field.failures')}: </dt>
                            <dd className="inline">
                              {r.consecutive_failures} / {r.max_consecutive_failures}
                            </dd>
                          </div>
                          <div>
                            <dt className="inline">{t('responsibilities.field.stopAt')}: </dt>
                            <dd className="inline">{r.stop_at}</dd>
                          </div>
                        </dl>
                        <div className="flex flex-wrap gap-2">
                          <Button size="sm" variant="outline" onClick={() => void openDetail(r.responsibility_id)}>
                            {t('responsibilities.action.detail')}
                          </Button>
                          {operable &&
                            controlsFor(r.state)
                              .filter((a) => !featureOff || a === 'pause' || a === 'disable')
                              .map((a) => (
                                <Button
                                  key={a}
                                  size="sm"
                                  variant={a === 'disable' ? 'ghost' : 'outline'}
                                  disabled={busy === r.responsibility_id}
                                  onClick={() => void control(r, a)}
                                >
                                  {a === 'pause' ? (
                                    <Pause className="size-3.5" />
                                  ) : a === 'resume' ? (
                                    <Play className="size-3.5" />
                                  ) : (
                                    <Power className="size-3.5" />
                                  )}
                                  {t(`responsibilities.action.${a}`)}
                                </Button>
                              ))}
                          {operable && isManager && !featureOff && r.consecutive_failures > 0 && (
                            <Button
                              size="sm"
                              variant="ghost"
                              disabled={busy === r.responsibility_id}
                              onClick={() => void control(r, 'clear_failures')}
                            >
                              <RotateCcw className="size-3.5" />
                              {t('responsibilities.action.clear_failures')}
                            </Button>
                          )}
                        </div>
                      </CardContent>
                    </Card>
                  );
                })}
              </section>
            ))}
          </div>
        )}
      </div>

      <Dialog open={selectedRow !== null} onOpenChange={(o) => !o && setSelected(null)}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>{selectedRow?.objective}</DialogTitle>
            <DialogDescription>{t('responsibilities.detail.desc')}</DialogDescription>
          </DialogHeader>
          {selectedRow && (
            <div className="max-h-[60vh] space-y-3 overflow-y-auto text-sm">
              <p className="whitespace-pre-wrap text-muted-foreground">{selectedRow.acceptance_template}</p>
              {selectedSummary?.open_occurrence && (
                <div className="flex flex-wrap items-center gap-2 rounded-md border p-3">
                  <span>{t('responsibilities.detail.openRun')}</span>
                  <Link className="underline" to={`/tasks/${selectedSummary.open_occurrence.task_id}`}>
                    {selectedSummary.open_occurrence.task_id}
                  </Link>
                  {canOperate(selectedRow.owner_agent_id) && (
                    <StopTaskButton taskId={selectedSummary.open_occurrence.task_id} terminal={false} onStopped={() => void load()} />
                  )}
                </div>
              )}
              <h3 className="font-medium">{t('responsibilities.detail.recent')}</h3>
              {occurrences === null ? (
                <p className="text-muted-foreground">{t('responsibilities.detail.loading')}</p>
              ) : occurrences.length === 0 ? (
                <p className="text-muted-foreground">{t('responsibilities.detail.none')}</p>
              ) : (
                <ul className="space-y-1" data-testid="responsibility-occurrences">
                  {occurrences.map((o) => (
                    <li key={o.task_id} className="flex flex-wrap gap-2 text-xs">
                      <span className="text-muted-foreground">{o.created_at}</span>
                      <Link className="underline" to={`/tasks/${o.task_id}`}>
                        {o.task_id}
                      </Link>
                      <Badge variant="secondary">{o.outcome ?? t('responsibilities.detail.running')}</Badge>
                      <span>{cents(o.charged_cents ?? o.reserved_cents)}</span>
                    </li>
                  ))}
                </ul>
              )}
              {selectedSummary && (
                <p className="text-xs text-muted-foreground">
                  {t('responsibilities.detail.notCounted', { n: selectedSummary.cost_not_counted.length })}
                </p>
              )}
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setSelected(null)}>
              {t('common.close')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {creating && (
        <CreateDialog
          agents={isAdmin ? null : operableAgents}
          maxDays={status?.max_stop_at_days ?? 30}
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            void load();
          }}
        />
      )}
    </div>
  );
}

function CreateDialog({
  agents,
  maxDays,
  onClose,
  onCreated,
}: {
  /** `null` ⇒ admin: free-text employee id (the server validates it). */
  agents: string[] | null;
  maxDays: number;
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [preset, setPreset] = useState<PresetId>('daily_briefing');
  const [form, setForm] = useState<PresetForm>({
    ownerAgentId: agents?.[0] ?? '',
    objective: '',
    acceptance: '',
    time: '08:00',
    timezone: localTimezone(),
    readOnly: true,
    runCapCents: 50,
    periodCapCents: 50,
    stopAfterDays: Math.min(30, maxDays),
  });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const applyPreset = (p: PresetId) => {
    setPreset(p);
    if (p === 'custom') return;
    setForm((f) => ({
      ...f,
      readOnly: true,
      objective: f.objective || t(`responsibilities.preset.${p}.objective`),
      acceptance: f.acceptance || t(`responsibilities.preset.${p}.acceptance`),
    }));
  };

  useEffect(() => {
    applyPreset('daily_briefing');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.responsibilities.create(buildInput(preset, { ...form, stopAfterDays: Math.min(form.stopAfterDays, maxDays) }));
      onCreated();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const set = <K extends keyof PresetForm>(k: K, v: PresetForm[K]) => setForm((f) => ({ ...f, [k]: v }));

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('responsibilities.create.title')}</DialogTitle>
          <DialogDescription>{t('responsibilities.create.desc')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3 text-sm">
          <label className="block space-y-1">
            <span>{t('responsibilities.create.preset')}</span>
            <select
              aria-label={t('responsibilities.create.preset')}
              className="w-full rounded-md border bg-background p-2"
              value={preset}
              onChange={(e) => applyPreset(e.target.value as PresetId)}
            >
              <option value="daily_briefing">{t('responsibilities.preset.daily_briefing')}</option>
              <option value="weekly_review">{t('responsibilities.preset.weekly_review')}</option>
              <option value="custom">{t('responsibilities.preset.custom')}</option>
            </select>
          </label>
          <label className="block space-y-1">
            <span>{t('responsibilities.create.employee')}</span>
            {agents === null ? (
              <Input value={form.ownerAgentId} onChange={(e) => set('ownerAgentId', e.target.value)} />
            ) : (
              <select
                aria-label={t('responsibilities.create.employee')}
                className="w-full rounded-md border bg-background p-2"
                value={form.ownerAgentId}
                onChange={(e) => set('ownerAgentId', e.target.value)}
              >
                {agents.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
            )}
          </label>
          <label className="block space-y-1">
            <span>{t('responsibilities.create.objective')}</span>
            <Textarea className="h-16" value={form.objective} onChange={(e) => set('objective', e.target.value)} />
          </label>
          <label className="block space-y-1">
            <span>{t('responsibilities.create.acceptance')}</span>
            <Textarea className="h-16" value={form.acceptance} onChange={(e) => set('acceptance', e.target.value)} />
          </label>
          <div className="grid grid-cols-2 gap-3">
            <label className="block space-y-1">
              <span>{t('responsibilities.create.time')}</span>
              <Input type="time" value={form.time} onChange={(e) => set('time', e.target.value)} />
            </label>
            <label className="block space-y-1">
              <span>{t('responsibilities.create.timezone')}</span>
              <Input value={form.timezone} onChange={(e) => set('timezone', e.target.value)} />
            </label>
            <label className="block space-y-1">
              <span>{t('responsibilities.create.runCap')}</span>
              <Input
                type="number"
                min={1}
                value={form.runCapCents}
                onChange={(e) => set('runCapCents', Number(e.target.value))}
              />
            </label>
            <label className="block space-y-1">
              <span>{t('responsibilities.create.periodCap')}</span>
              <Input
                type="number"
                min={1}
                value={form.periodCapCents}
                onChange={(e) => set('periodCapCents', Number(e.target.value))}
              />
            </label>
            <label className="block space-y-1">
              <span>{t('responsibilities.create.stopAfter', { max: maxDays })}</span>
              <Input
                type="number"
                min={1}
                max={maxDays}
                value={form.stopAfterDays}
                onChange={(e) => set('stopAfterDays', Number(e.target.value))}
              />
            </label>
          </div>
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={preset !== 'custom' || form.readOnly}
              disabled={preset !== 'custom'}
              onChange={(e) => set('readOnly', e.target.checked)}
            />
            <span>
              {t('responsibilities.create.readOnly')}
              <span className="block text-xs text-muted-foreground">{t('responsibilities.create.readOnlyHint')}</span>
            </span>
          </label>
          {error && <p className="text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button disabled={busy || !form.ownerAgentId || !form.objective.trim()} onClick={() => void submit()}>
            {t('responsibilities.action.create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export default ResponsibilitiesPage;
