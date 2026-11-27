import { useAuthStore } from '@/stores/auth-store';
import { DataExportButton } from '@/components/DataExportButton';
import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type BillingUsage, type BudgetIncident, type BudgetByAgent, type DepartmentCostSummary, type DepartmentCostTotals, type CostRecentResult } from '@/lib/api';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import { useUrlState } from '@/lib/use-url-state';
import { Wallet, GaugeCircle, ShieldAlert } from 'lucide-react';
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Badge,
  Empty,
  ErrorState,
  Segmented,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  ActorAvatar,
  CrossLink,
} from '@/components/mds';

/** A single usage KPI tile: label, big value, `used / limit` sub, progress bar. */
function UsageTile({
  label,
  used,
  limit,
}: {
  readonly label: string;
  readonly used: number;
  readonly limit: number;
}) {
  const intl = useIntl();
  const unlimited = limit < 0;
  const pct = unlimited ? 0 : limit > 0 ? Math.min((used / limit) * 100, 100) : 0;
  const barColor = pct >= 90 ? 'bg-destructive' : pct >= 70 ? 'bg-warning' : 'bg-success';

  return (
    <div className="space-y-2 p-4">
      <p className="text-sm text-muted-foreground">{label}</p>
      <p className="text-2xl font-semibold tabular-nums">{used.toLocaleString()}</p>
      <p className="font-mono text-xs tabular-nums text-muted-foreground">
        {used.toLocaleString()}
        {' / '}
        {unlimited ? intl.formatMessage({ id: 'billing.unlimited' }) : limit.toLocaleString()}
      </p>
      <div className="h-2 overflow-hidden rounded-full bg-muted">
        <div
          className={cn('h-full rounded-full transition-all duration-500', barColor)}
          style={{ width: unlimited ? '0%' : `${pct}%` }}
        />
      </div>
    </div>
  );
}

/** Format a cent amount as a USD string (e.g. 1234 → $12.34). */
function fmtCents(cents: number): string {
  return `$${(cents / 100).toFixed(2)}`;
}

const INCIDENT_FILTERS = ['all', 'over'] as const;

// ── Budget console (WP14-T14.6) — open-events per AI staff + recent incidents ──

function BudgetConsole() {
  const intl = useIntl();
  const [incidents, setIncidents] = useState<BudgetIncident[]>([]);
  const [byAgent, setByAgent] = useState<BudgetByAgent[]>([]);
  const [loading, setLoading] = useState(true);
  // P11 (state-as-URL): "show me only the over-budget incidents" survives a
  // refresh and can be sent to someone else as a link.
  const [filter, setFilter] = useUrlState('incidents', 'all', { allowed: INCIDENT_FILTERS });
  // P05: an empty incident list is genuinely good news ("no budget events"),
  // so a failed fetch falling into the same branch tells the user the exact
  // opposite of the truth on a spend-safety screen.
  const [loadError, setLoadError] = useState<unknown>(null);

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api.budget
      .incidents(50)
      .then((res) => {
        setIncidents(res?.incidents ?? []);
        setByAgent(res?.by_agent ?? []);
      })
      .catch((e) => {
        console.warn('[api]', e);
        setLoadError(e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      })
      .finally(() => setLoading(false));
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const openAgents = byAgent.filter((a) => a.open_events > 0);
  const isOver = (inc: BudgetIncident) => inc.cap_cents > 0 && inc.spent_cents > inc.cap_cents;
  const filtered = filter === 'over' ? incidents.filter(isOver) : incidents;

  return (
    <section className="space-y-4">
      <div className="flex items-center gap-2">
        <GaugeCircle className="size-5 text-muted-foreground" />
        <h2 className="text-base font-medium">{intl.formatMessage({ id: 'billing.budgetConsole' })}</h2>
      </div>

      {loading ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : loadError != null ? (
        <ErrorState
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
          onRetry={load}
        />
      ) : incidents.length === 0 && openAgents.length === 0 ? (
        <Empty
          icon={GaugeCircle}
          title={intl.formatMessage({ id: 'billing.budgetConsole.empty' })}
          description={intl.formatMessage({ id: 'billing.budgetConsole.emptyHint' })}
        />
      ) : (
        <div className="space-y-5">
          {/* Per-agent open-event summary */}
          {openAgents.length > 0 && (
            <div className="space-y-2">
              <p className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'billing.budgetConsole.openByAgent' })}
              </p>
              <div className="flex flex-wrap gap-2">
                {openAgents.map((a) => (
                  <Badge key={a.agent_id} variant="secondary" className="bg-warning/15 text-warning">
                    <ActorAvatar actorType="agent" name={a.agent_id} size="xs" />
                    {a.agent_id}
                    <span className="ml-1 font-mono font-semibold">{a.open_events}</span>
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {/* Recent incidents */}
          {incidents.length > 0 && (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'billing.budgetConsole.recent' })}
                </p>
                <Segmented
                  value={filter}
                  onValueChange={setFilter}
                  aria-label={intl.formatMessage({ id: 'billing.budgetConsole.recent' })}
                  options={[
                    { value: 'all', label: intl.formatMessage({ id: 'billing.incidents.filter.all' }) },
                    { value: 'over', label: intl.formatMessage({ id: 'billing.incidents.filter.over' }) },
                  ]}
                />
              </div>
              <div className="overflow-hidden rounded-xl border border-surface-border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.agent' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.event' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.scope' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.spent' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.time' })}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filtered.map((inc, i) => {
                      const over = isOver(inc);
                      return (
                        <TableRow key={`${inc.ts}-${inc.agent_id}-${i}`}>
                          <TableCell>
                            <span className="flex items-center gap-2">
                              <ActorAvatar actorType="agent" name={inc.agent_id} size="xs" />
                              <span className="font-medium">{inc.agent_id}</span>
                            </span>
                          </TableCell>
                          <TableCell>
                            <Badge variant="outline">{inc.event}</Badge>
                          </TableCell>
                          <TableCell className="text-xs text-muted-foreground">{inc.scope}</TableCell>
                          <TableCell className={cn('font-mono text-xs', over && 'text-destructive')}>
                            {fmtCents(inc.spent_cents)}
                            <span className="text-muted-foreground"> / {fmtCents(inc.cap_cents)}</span>
                          </TableCell>
                          <TableCell className="font-mono text-xs text-muted-foreground">
                            {new Date(inc.ts).toLocaleString('zh-TW', {
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
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

/** Read-only departmental usage, using only departments granted by users.me. */
function DepartmentUsage({ departments }: { departments: string[] }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const connection = useConnectionStore(s => s.state);
  const [chosen, setChosen] = useState(departments[0]);
  const department = departments.includes(chosen) ? chosen : departments[0];
  const [days, setDays] = useState<7 | 30 | 90>(30);
  const [result, setResult] = useState<{ key: string; summary: DepartmentCostSummary; recent: CostRecentResult }>();
  const [error, setError] = useState<{ key: string; value: unknown }>();
  const [retry, setRetry] = useState(0);
  const key = JSON.stringify([department, days, connection, retry]);
  useEffect(() => {
    if (connection !== 'authenticated' || !department) return;
    let current = true;
    Promise.all([api.cost.departmentSummary(department, days), api.cost.departmentRecent(department)])
      .then(([summary, recent]) => { if (current) setResult({ key, summary, recent }); })
      .catch(value => { if (current) setError({ key, value }); });
    return () => { current = false; };
  }, [department, days, connection, key]);
  const data = result?.key === key ? result : undefined;
  const money = (millicents: number) => fmtCents(millicents / 1000);
  const columns = ['billing.department.requests', 'billing.department.input', 'billing.department.output',
    'billing.department.cost', 'billing.department.savings', 'billing.department.efficiency'];
  const cells = (row: DepartmentCostTotals) => <>
    <TableCell>{intl.formatNumber(row.total_requests)}</TableCell>
    <TableCell>{intl.formatNumber(row.total_input_tokens)}</TableCell>
    <TableCell>{intl.formatNumber(row.total_output_tokens)}</TableCell>
    <TableCell>{money(row.total_cost_millicents)}</TableCell>
    <TableCell>{money(row.total_cache_savings_millicents)}</TableCell>
    <TableCell>{intl.formatNumber(row.avg_cache_efficiency, { style: 'percent', maximumFractionDigits: 1 })}</TableCell>
  </>;
  return <section className="space-y-4" aria-label={t('billing.department.title')}>
    <h2 className="text-base font-medium">{t('billing.department.title')}</h2>
    <div className="flex flex-wrap gap-4">
      <label className="space-y-1 text-sm">
        <span className="block">{t('department.filter')}</span>
        <select className="rounded-lg border border-input bg-surface px-3 py-2 focus-visible:outline-2 focus-visible:outline-ring" value={department} onChange={e => setChosen(e.target.value)}>
          {departments.map(d => <option key={d} value={d}>{d}</option>)}
        </select>
      </label>
      <label className="space-y-1 text-sm">
        <span className="block">{t('billing.department.period')}</span>
        <select className="rounded-lg border border-input bg-surface px-3 py-2 focus-visible:outline-2 focus-visible:outline-ring" value={days} onChange={e => setDays(Number(e.target.value) as 7 | 30 | 90)}>
          {([7, 30, 90] as const).map(d => <option key={d} value={d}>{intl.formatMessage({ id: 'billing.department.days' }, { days: d })}</option>)}
        </select>
      </label>
    </div>
    {error?.key === key ? <ErrorState error={error.value} title={t('errorState.manage.loadFailed')} onRetry={() => setRetry(n => n + 1)} />
      : !data ? <p role="status" className="text-sm text-muted-foreground">{t('common.loading')}</p>
      : <>
        {!data.summary?.available ? <p className="text-sm text-muted-foreground">{t('billing.department.unavailable')}</p>
          : data.summary.agents.length === 0 ? <p className="text-sm text-muted-foreground">{t('billing.department.empty')}</p>
          : <div className="overflow-x-auto rounded-xl border border-surface-border">
            <Table>
              <TableHeader><TableRow><TableHead>{t('billing.col.agent')}</TableHead>{columns.map(c => <TableHead key={c}>{t(c)}</TableHead>)}</TableRow></TableHeader>
              <TableBody>
                {data.summary.agents.map(a => <TableRow key={a.agent_id}><TableCell>{a.display_name || a.agent_id}</TableCell>{cells(a)}</TableRow>)}
                <TableRow className="font-medium"><TableCell>{t('billing.department.total')}</TableCell>{cells(data.summary.totals)}</TableRow>
              </TableBody>
            </Table>
          </div>}
        <h3 className="text-sm font-medium">{t('billing.department.recent')}</h3>
        {!data.recent?.available ? <p className="text-sm text-muted-foreground">{t('billing.department.unavailable')}</p>
          : data.recent.records.length === 0 ? <p className="text-sm text-muted-foreground">{t('billing.department.noRecent')}</p>
          : <ul className="divide-y divide-surface-border rounded-xl border border-surface-border">
            {data.recent.records.map((r, i) => <li key={`${r.agent_id}-${r.created_at}-${i}`} className="flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-3 text-sm">
              <span>{data.summary?.agents?.find(a => a.agent_id === r.agent_id)?.display_name || r.agent_id}</span>
              <span className="text-muted-foreground">{r.model}</span>
              <span className="tabular-nums">{money(r.cost_millicents)}</span>
              <time className="text-xs text-muted-foreground" dateTime={r.created_at}>{intl.formatDate(r.created_at, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}</time>
            </li>)}
          </ul>}
      </>}
  </section>;
}

// ── Spend-cap mechanisms overview (WP-D §2-4) ──────────────────────
//
// UX audit §2-4 — five independent "how much can this cost" mechanisms
// (account budget / per-employee budget / governance quota / kill-switch
// cost limit / the budget circuit-breaker events surfaced only here) live on
// five different pages with no cross-links and no shared vocabulary. This
// page is pure read-only (no cap-setting control anywhere on it), so instead
// of adding editing here (which would just create a sixth write point) this
// card is a plain-language map: one sentence per mechanism + a link to where
// it is actually configured (R-EDIT-SOURCE).
function SpendCapsOverview() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();

  const rows: { key: string; linkLabelId: string; to: string }[] = [
    { key: 'account', linkLabelId: 'billing.caps.account.link', to: '/app/system/accounts' },
    { key: 'agent', linkLabelId: 'billing.caps.agent.link', to: '/agents' },
    // G2 (2026-09 feature audit): the "governance quota" row was removed with
    // the Governance Layer — its daily-token-budget policy had no enforcer.
    { key: 'killswitch', linkLabelId: 'billing.caps.killswitch.link', to: '/app/system/security' },
    { key: 'incidents', linkLabelId: 'billing.caps.incidents.link', to: '/inbox' },
  ];

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ShieldAlert className="size-4 text-muted-foreground" />
          {t('billing.caps.title')}
        </CardTitle>
      </CardHeader>
      <CardContent className="p-0">
        <ul className="divide-y divide-surface-border">
          {rows.map((row) => (
            <li key={row.key} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
              <div className="min-w-0">
                <p className="text-sm font-medium text-foreground">{t(`billing.caps.${row.key}.title`)}</p>
                <p className="text-xs text-muted-foreground">{t(`billing.caps.${row.key}.desc`)}</p>
              </div>
              <CrossLink label={t(row.linkLabelId)} onClick={() => navigate(row.to)} />
            </li>
          ))}
        </ul>
      </CardContent>
    </Card>
  );
}

export function BillingPage() {
  const intl = useIntl();
  const { user, deptGrants } = useAuthStore();
  const costDepartments = [...new Set(deptGrants.filter(g => g.groups.includes('cost')).map(g => g.department))];
  const showDepartment = !!user && user.role !== 'admin' && costDepartments.length > 0;
  const connectionState = useConnectionStore((s) => s.state);
  const [usage, setUsage] = useState<BillingUsage | null>(null);
  // P05: every usage tile falls back to `?? 0`, so a failed fetch renders a
  // full dashboard reading "0 / 0" — a plausible, wrong, and reassuring number.
  const [usageError, setUsageError] = useState<unknown>(null);

  const loadUsage = useCallback(() => {
    setUsageError(null);
    api.billing
      .usage()
      .then((res) => setUsage(res))
      .catch((e) => {
        console.warn('[api]', e);
        setUsageError(e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
  }, [intl]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    loadUsage();
  }, [connectionState, loadUsage]);

  return (
    <div className="mx-auto w-full max-w-6xl space-y-5">
      {/* Header */}
      <div className="flex min-w-0 items-center gap-2">
        <Wallet className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'billing.title' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'billing.subtitle' })}</p>
        </div>
      </div>

      {showDepartment && <DepartmentUsage key={JSON.stringify([user?.id, costDepartments])} departments={costDepartments} />}

      <DataExportButton dataset="usage" />

      {/* Spend-cap mechanisms overview — R-EDIT-SOURCE map, WP-D §2-4 */}
      <SpendCapsOverview />

      {/* Usage KPI group */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'billing.usage' })}</CardTitle>
        </CardHeader>
        <CardContent className="p-0">
          {/* No usage payload + an error ⇒ show the error instead of a grid of
              zeros; a stale payload keeps its numbers with the error above. */}
          {usageError != null && (
            <div className="border-t border-surface-border p-4">
              <ErrorState
                variant={usage == null ? 'page' : 'inline'}
                error={usageError}
                title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
                description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
                onRetry={loadUsage}
              />
            </div>
          )}
          {!(usageError != null && usage == null) && (
          <div className="grid grid-cols-2 divide-x divide-y divide-surface-border border-t border-surface-border lg:grid-cols-4">
            <UsageTile
              label={intl.formatMessage({ id: 'billing.conversations' })}
              used={usage?.conversations?.used ?? 0}
              limit={usage?.conversations?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.agents' })}
              used={usage?.agents?.used ?? 0}
              limit={usage?.agents?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.channels' })}
              used={usage?.channels?.used ?? 0}
              limit={usage?.channels?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.inferenceHours' })}
              used={usage?.inference_hours?.used ?? 0}
              limit={usage?.inference_hours?.limit ?? 0}
            />
          </div>
          )}
        </CardContent>
      </Card>

      {/* Budget console — open budget events + recent incidents (WP14-T14.6) */}
      <BudgetConsole />
    </div>
  );
}
