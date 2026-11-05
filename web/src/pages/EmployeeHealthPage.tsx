import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Activity, RefreshCw } from 'lucide-react';
import { ActorAvatar, Badge, Button, Card, CardContent, Empty, PageHeader, Segmented, Skeleton } from '@/components/mds';
import { HEALTH_FAILURE_REASONS, loadEmployeeHealth, type EmployeeHealth, type EmployeeHealthSnapshot } from '@/lib/employee-health';
import { formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';

function healthState(agent: EmployeeHealth): 'healthy' | 'attention' | 'unknown' {
  if (agent.consecutive_failures > 0 || agent.paused_goals > 0 || agent.account_status === 'degraded') return 'attention';
  return agent.last_success_at && agent.account_status === 'available' ? 'healthy' : 'unknown';
}

export function EmployeeHealthPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const userId = useAuthStore(s => s.user?.id);
  const userRole = useAuthStore(s => s.user?.role);
  const [windowDays, setWindowDays] = useState<1 | 7>(1);
  const [revision, setRevision] = useState(0);
  const [snapshot, setSnapshot] = useState<EmployeeHealthSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  useEffect(() => {
    let alive = true;
    let pending = false;
    setSnapshot(null);
    const refresh = async () => {
      if (pending) return;
      pending = true;
      setLoading(true);
      try {
        const result = await loadEmployeeHealth(windowDays);
        if (alive) { setSnapshot(result); setError(''); }
      } catch (e) {
        if (alive) { setSnapshot(null); setError(formatError(e)); }
      } finally {
        pending = false;
        if (alive) setLoading(false);
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 30_000);
    return () => { alive = false; window.clearInterval(timer); };
  }, [windowDays, revision, userId, userRole]);
  const priority = { attention: 0, unknown: 1, healthy: 2 };
  const agents = [...(snapshot?.agents ?? [])].sort((left, right) => priority[healthState(left)] - priority[healthState(right)]);
  const count = (state: ReturnType<typeof healthState>) => agents.filter(agent => healthState(agent) === state).length;
  return <div className="flex min-h-0 flex-1 flex-col">
    <PageHeader className="h-auto min-h-12 flex-wrap py-2">
      <Activity aria-hidden="true" className="size-4 text-success" />
      <h1 className="mr-auto text-sm font-medium">{t('health.title')}</h1>
      <Segmented value={String(windowDays)} onValueChange={value => setWindowDays(value === '7' ? 7 : 1)} aria-label={t('health.window')}
        options={[{ value: '1', label: t('health.day') }, { value: '7', label: t('health.week') }]} />
      <Button variant="ghost" size="icon" disabled={loading} aria-label={t('health.refresh')} onClick={() => setRevision(value => value + 1)}>
        <RefreshCw aria-hidden="true" className="size-4" />
      </Button>
    </PageHeader>
    <main className="mx-auto w-full max-w-6xl space-y-5 overflow-y-auto p-4 md:p-6">
      <p className="max-w-3xl text-sm leading-6 text-muted-foreground">{t('health.description')}</p>
      {error ? <div role="alert"><Empty icon={Activity} tone="destructive" title={t('health.error')} description={error}
        action={<Button variant="outline" onClick={() => setRevision(value => value + 1)}>{t('health.retry')}</Button>} /></div>
        : !snapshot ? <div role="status" aria-label={t('health.loading')} className="space-y-3"><Skeleton className="h-24 w-full" /><Skeleton className="h-40 w-full" /></div>
        : <>
          {snapshot.partial && <p role="status" className="rounded-lg border border-warning/30 bg-warning/10 p-3 text-sm">{t('health.partial')}</p>}
          <section aria-label={t('health.summary')} className="grid gap-3 sm:grid-cols-3">
            {(['healthy', 'attention', 'unknown'] as const).map(state => <Card key={state}><CardContent className="p-4">
              <p className="text-sm text-muted-foreground">{t(`health.state.${state}`)}</p>
              <p className={`mt-2 text-3xl font-semibold tabular-nums ${state === 'healthy' ? 'text-success' : ''}`}>{count(state)}</p>
            </CardContent></Card>)}
          </section>
          {agents.length === 0 ? <Empty icon={Activity} title={t('health.empty')} description={t('health.emptyDescription')} />
            : <section aria-label={t('health.employees')} className="space-y-3">
              {agents.map(agent => {
                const state = healthState(agent);
                const lastSuccess = agent.last_success_at ? new Date(agent.last_success_at) : null;
                const lastSuccessLabel = lastSuccess && Number.isFinite(lastSuccess.getTime())
                  ? intl.formatDate(lastSuccess, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
                  : t('health.noSuccess');
                return <Card key={agent.id}><CardContent className="space-y-4 p-4">
                  <div className="flex flex-wrap items-center gap-3">
                    <ActorAvatar actorType="agent" name={agent.name} size="lg" />
                    <Link to={`/agents/${encodeURIComponent(agent.id)}`} className="min-w-0 flex-1 truncate font-medium hover:underline focus-visible:ring-3 focus-visible:ring-ring/50" title={agent.name}>{agent.name}</Link>
                    <Badge variant="secondary" className={state === 'healthy' ? 'bg-success/15 text-success' : state === 'attention' ? 'bg-warning/15 text-warning' : ''}>{t(`health.state.${state}`)}</Badge>
                  </div>
                  <dl className="grid gap-4 text-sm sm:grid-cols-3">
                    <div><dt className="text-muted-foreground">{t('health.lastSuccess')}</dt><dd className="mt-1">{lastSuccessLabel}</dd></div>
                    <div><dt className="text-muted-foreground">{t('health.consecutive')}</dt><dd className="mt-1 tabular-nums">{agent.consecutive_failures}</dd></div>
                    <div><dt className="text-muted-foreground">{t('health.pausedGoals')}</dt><dd className="mt-1 tabular-nums">{agent.paused_goals}</dd></div>
                  </dl>
                  <details className="border-t border-surface-border pt-3">
                    <summary className="cursor-pointer rounded text-sm text-muted-foreground focus-visible:ring-3 focus-visible:ring-ring/50">{t('health.details')}</summary>
                    <div className="mt-4 space-y-4">
                      <p className="text-sm">{t('health.account')}{'：'}{t(`health.account.${['available', 'degraded'].includes(agent.account_status) ? agent.account_status : 'unknown'}`)}</p>
                      <dl className="grid grid-cols-2 gap-3 text-sm md:grid-cols-4">{HEALTH_FAILURE_REASONS.map(reason => <div key={reason}>
                        <dt className="text-muted-foreground">{t(`health.reason.${reason}`)}</dt><dd className="mt-1 tabular-nums">{agent.failures[reason] ?? 0}</dd>
                      </div>)}</dl>
                      <p className="text-xs text-muted-foreground">{t('health.failureNote')}</p>
                      <Link to={`/agents/${encodeURIComponent(agent.id)}/edit`} className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-3 focus-visible:ring-ring/50">{t('health.editEmployee')}</Link>
                    </div>
                  </details>
                </CardContent></Card>;
              })}
            </section>}
        </>}
    </main>
  </div>;
}
