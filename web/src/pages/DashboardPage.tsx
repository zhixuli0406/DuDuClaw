import { useEffect, useState, type ComponentType } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { useConnectionStore } from '@/stores/connection-store';
import {
  api,
  type BudgetSummary,
  type DoctorCheck,
  type WikiPageMeta,
  type WikiStats,
} from '@/lib/api';
import { WikiGraph } from '@/components/WikiGraph';
import { ActivityFeed } from '@/components/ActivityFeed';
import { IncidentBanner } from '@/components/IncidentBanner';
import { useTasksStore } from '@/stores/tasks-store';
import type { TaskStatus } from '@/lib/api';
import {
  Bot,
  Radio,
  Wallet,
  HeartPulse,
  BookOpen,
  FileText,
  Clock,
  ExternalLink,
  KanbanSquare,
  AlertCircle,
  CheckCircle2,
  Ban,
  LayoutDashboard,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  Badge,
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  CardContent,
  Skeleton,
} from '@/components/mds';

/** KPI tile (spec §5.5): label, big value, optional hint; tinted icon. */
function StatTile({
  icon: Icon,
  tone,
  label,
  value,
  hint,
}: {
  icon: ComponentType<{ className?: string }>;
  tone: 'brand' | 'success' | 'warning' | 'danger';
  label: string;
  value: string | number;
  hint?: string;
}) {
  const toneClass =
    tone === 'success'
      ? 'text-success'
      : tone === 'warning'
        ? 'text-warning'
        : tone === 'danger'
          ? 'text-destructive'
          : 'text-brand';
  return (
    <div className="rounded-lg border border-surface-border bg-card p-4">
      <div className="flex items-center gap-2">
        <Icon className={cn('size-4', toneClass)} />
        <p className="text-sm text-muted-foreground">{label}</p>
      </div>
      <p className="mt-2 text-2xl font-semibold tabular-nums text-foreground">{value}</p>
      {hint && <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

// ── Task Board mini-preview (4-column Kanban summary) ────────

const PREVIEW_COLUMNS: ReadonlyArray<{
  status: TaskStatus;
  icon: ComponentType<{ className?: string }>;
  accent: string;
}> = [
  { status: 'todo', icon: Clock, accent: 'border-t-muted-foreground/40' },
  { status: 'in_progress', icon: AlertCircle, accent: 'border-t-brand' },
  { status: 'done', icon: CheckCircle2, accent: 'border-t-success' },
  { status: 'blocked', icon: Ban, accent: 'border-t-destructive' },
];

function TasksPreviewCard() {
  const intl = useIntl();
  const tasks = useTasksStore((s) => s.tasks);
  const loading = useTasksStore((s) => s.loading);
  const error = useTasksStore((s) => s.error);
  const fetchTasks = useTasksStore((s) => s.fetchTasks);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  const byStatus = (status: TaskStatus) => tasks.filter((t) => t.status === status);
  const isEmpty = tasks.length === 0;
  // Distinguish "never loaded yet" from "loaded empty" to avoid rendering a
  // confusing empty state before the first fetch returns.
  const notYetLoaded = loading && isEmpty;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <KanbanSquare className="size-4 text-brand" />
          {intl.formatMessage({ id: 'tasks.preview.title' })}
        </CardTitle>
        <CardAction>
          <Link
            to="/tasks"
            className="flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-brand"
          >
            {intl.formatMessage({ id: 'tasks.preview.viewAll' })}
            <ExternalLink className="size-3" />
          </Link>
        </CardAction>
      </CardHeader>
      <CardContent>
        {error && !loading && (
          <div className="mb-3 rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">{error}</div>
        )}

        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          {PREVIEW_COLUMNS.map(({ status, icon: Icon, accent }) => {
            const colTasks = byStatus(status).slice(0, 3);
            return (
              <div key={status} className={cn('rounded-lg border-t-2 bg-muted/50 p-3', accent)}>
                <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-foreground">
                  <Icon className="size-3.5" />
                  <span>{intl.formatMessage({ id: `tasks.column.${status}` })}</span>
                  <span className="ml-auto rounded-full bg-muted px-1.5 py-0.5 text-[10px] tabular-nums text-muted-foreground">
                    {byStatus(status).length}
                  </span>
                </div>
                <div className="space-y-1.5">
                  {notYetLoaded ? (
                    <>
                      <Skeleton className="h-6 w-full rounded" />
                      <Skeleton className="h-6 w-full rounded" />
                    </>
                  ) : (
                    <>
                      {colTasks.map((t) => (
                        <div
                          key={t.id}
                          className="truncate rounded border border-surface-border bg-surface px-2 py-1 text-xs text-foreground"
                          title={t.title}
                        >
                          {t.title}
                        </div>
                      ))}
                      {colTasks.length === 0 && (
                        <div className="py-1 text-center text-[10px] text-muted-foreground/60">—</div>
                      )}
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </div>

        {isEmpty && !loading && !error && (
          <p className="mt-3 text-center text-xs text-muted-foreground/60">
            {intl.formatMessage({ id: 'tasks.preview.empty' })}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

export function DashboardPage() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const { status, fetchStatus } = useSystemStore();
  const connectionState = useConnectionStore((s) => s.state);
  const [budget, setBudget] = useState<BudgetSummary | null>(null);
  const [doctor, setDoctor] = useState<{
    checks: DoctorCheck[];
    summary: { pass: number; warn: number; fail: number };
  } | null>(null);
  const [wikiPages, setWikiPages] = useState<ReadonlyArray<WikiPageMeta>>([]);
  const [wikiStats, setWikiStats] = useState<WikiStats | null>(null);
  const [wikiContents, setWikiContents] = useState<Record<string, string>>({});
  // Pending approvals + open budget events feed the IncidentBanner chip.
  // Both are manager-gated RPCs — failures are swallowed so an employee's
  // dashboard simply shows no approval chip.
  const [approvalsCount, setApprovalsCount] = useState(0);

  // Fetch data only after WebSocket is authenticated.
  // Re-fetches on reconnect (connectionState goes back to 'authenticated').
  useEffect(() => {
    if (connectionState !== 'authenticated') return;

    fetchAgents();
    fetchStatus();
    api.accounts.budgetSummary().then(setBudget).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    api.system.doctor().then(setDoctor).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });

    // Approval + budget incident counts for the banner. Silent on failure
    // (manager-gated; non-privileged users just see no chip).
    Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
    ]).then(([approvals, budget]) => {
      const pending = approvals?.count ?? 0;
      const openBudget = budget?.by_agent?.length ?? 0;
      setApprovalsCount(pending + openBudget);
    }).catch(() => { /* silent */ });

    // Fetch wiki data for the default (first) agent
    api.agents.list().then(async (res) => {
      const agentList = res?.agents ?? [];
      if (agentList.length === 0) return;
      const mainAgent = agentList[0].name;

      const [pagesRes, statsRes] = await Promise.all([
        api.wiki.pages(mainAgent).catch(() => null),
        api.wiki.stats(mainAgent).catch(() => null),
      ]);

      if (pagesRes?.pages) setWikiPages(pagesRes.pages);
      if (statsRes) setWikiStats(statsRes);

      // Fetch contents for graph (batched, max 20 pages)
      if (pagesRes?.pages && pagesRes.pages.length > 0) {
        const contents: Record<string, string> = {};
        const pagesToFetch = pagesRes.pages.slice(0, 20);
        for (let i = 0; i < pagesToFetch.length; i += 5) {
          const batch = pagesToFetch.slice(i, i + 5);
          await Promise.all(
            batch.map(async (p) => {
              try {
                const r = await api.wiki.read(mainAgent, p.path);
                contents[p.path] = r?.content ?? '';
              } catch {
                /* skip */
              }
            })
          );
        }
        setWikiContents(contents);
      }
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });

    // Lightweight budget refresh every 60s. Silent on failure to avoid spamming
    // toasts from a transient blip — the initial load already surfaces errors.
    const interval = setInterval(() => {
      api.accounts.budgetSummary().then(setBudget).catch((e) => console.warn('[api]', e));
    }, 60_000);
    return () => clearInterval(interval);
  }, [connectionState, fetchAgents, fetchStatus]);

  const activeAgents = agents.filter((a) => a.status === 'active').length;
  const totalBudget = budget?.total_budget_cents ?? 0;
  const totalSpent = budget?.total_spent_cents ?? 0;

  const checks = doctor?.checks ?? [];
  const summary = doctor?.summary ?? { pass: 0, warn: 0, fail: 0 };
  const healthValue = doctor ? `${summary.pass}/${checks.length}` : '—';
  const healthSubtitle = doctor
    ? summary.fail > 0
      ? intl.formatMessage({ id: 'dashboard.health.failCount' }, { count: summary.fail })
      : summary.warn > 0
        ? intl.formatMessage({ id: 'dashboard.health.warnCount' }, { count: summary.warn })
        : intl.formatMessage({ id: 'dashboard.health.allPassed' })
    : intl.formatMessage({ id: 'common.loading' });

  return (
    <div className="space-y-6">
      {/* Slim page header (spec §5.2). */}
      <div className="flex items-center gap-2">
        <LayoutDashboard className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.dashboard' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'app.subtitle' })}</p>
        </div>
      </div>

      {/* Incident banner — silent unless something needs the owner's attention */}
      <IncidentBanner approvalsCount={approvalsCount} />

      {/* KPI tiles */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatTile
          icon={Bot}
          tone="brand"
          label={intl.formatMessage({ id: 'dashboard.agents.title' })}
          value={agents.length}
          hint={intl.formatMessage({ id: 'dashboard.agents.active' }, { count: activeAgents })}
        />
        <StatTile
          icon={Radio}
          tone="success"
          label={intl.formatMessage({ id: 'dashboard.channels.title' })}
          value={status?.channels_connected ?? 0}
          hint={intl.formatMessage(
            { id: 'dashboard.channels.connected' },
            { count: status?.channels_connected ?? 0 }
          )}
        />
        <StatTile
          icon={Wallet}
          tone="warning"
          label={intl.formatMessage({ id: 'dashboard.budget.title' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
          hint={`/ $${(totalBudget / 100).toFixed(2)}`}
        />
        <StatTile
          icon={HeartPulse}
          tone={summary.fail ? 'danger' : summary.warn ? 'warning' : 'success'}
          label={intl.formatMessage({ id: 'dashboard.health.title' })}
          value={healthValue}
          hint={healthSubtitle}
        />
      </div>

      {/* Doctor checks */}
      {checks.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>{intl.formatMessage({ id: 'dashboard.health.title' })}</CardTitle>
          </CardHeader>
          <div className="divide-y divide-surface-border">
            {checks.map((check) => (
              <div key={check.name} className="flex items-center justify-between gap-3 px-4 py-2.5">
                <span className="font-mono text-xs text-foreground">{check.name}</span>
                <div className="flex items-center gap-2.5">
                  <span className="truncate text-xs text-muted-foreground">{check.message}</span>
                  <span
                    className={cn(
                      'inline-block size-2 shrink-0 rounded-full',
                      check.status === 'pass'
                        ? 'bg-success'
                        : check.status === 'warn'
                          ? 'bg-warning'
                          : 'bg-destructive'
                    )}
                  />
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}

      {/* Wiki knowledge graph + recent pages */}
      {wikiStats?.exists && wikiPages.length > 0 && (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
          <Card className="lg:col-span-2">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <BookOpen className="size-4 text-brand" />
                {intl.formatMessage({ id: 'dashboard.wiki.graph' })}
              </CardTitle>
              <CardAction>
                <Link
                  to="/wiki"
                  className="flex items-center gap-1 text-xs text-brand hover:underline"
                >
                  {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                  <ExternalLink className="size-3" />
                </Link>
              </CardAction>
            </CardHeader>
            <CardContent>
              <WikiGraph pages={wikiPages} pageContents={wikiContents} width={650} height={350} />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <FileText className="size-4 text-brand" />
                {intl.formatMessage({ id: 'dashboard.wiki.recentPages' })}
              </CardTitle>
              <CardAction>
                <Badge variant="secondary" className="bg-brand/15 text-brand">
                  {wikiStats?.total_pages ?? 0}
                </Badge>
              </CardAction>
            </CardHeader>
            <CardContent>
              {wikiStats?.by_directory && (
                <div className="mb-4 flex flex-wrap gap-2">
                  {Object.entries(wikiStats.by_directory).map(([dir, count]) => (
                    <Badge key={dir} variant="secondary">
                      {dir}/ <span className="font-semibold tabular-nums">{count}</span>
                    </Badge>
                  ))}
                </div>
              )}

              <div className="space-y-1">
                {wikiPages.slice(0, 8).map((page) => (
                  <Link
                    key={page.path}
                    to="/wiki"
                    className="flex items-center justify-between rounded-lg px-3 py-2 text-sm transition-colors hover:bg-surface-hover"
                  >
                    <div className="flex min-w-0 items-center gap-2">
                      <FileText className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="truncate text-foreground">{page.title}</span>
                    </div>
                    <span className="ml-2 flex shrink-0 items-center gap-1 text-xs text-muted-foreground">
                      <Clock className="size-3" />
                      {new Date(page.updated).toLocaleDateString('zh-TW', {
                        month: 'short',
                        day: 'numeric',
                      })}
                    </span>
                  </Link>
                ))}
              </div>

              {wikiPages.length > 8 && (
                <Link
                  to="/wiki"
                  className="mt-3 flex items-center justify-center gap-1 rounded-lg bg-muted py-2 text-xs text-muted-foreground transition-colors hover:bg-surface-hover"
                >
                  {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                  <ExternalLink className="size-3" />
                </Link>
              )}
            </CardContent>
          </Card>
        </div>
      )}

      {/* Task board preview (team Kanban overview) */}
      <TasksPreviewCard />

      {/* Activity feed */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'activity.title' })}</CardTitle>
        </CardHeader>
        <CardContent>
          <ActivityFeed limit={10} showFilter agents={agents} />
        </CardContent>
      </Card>
    </div>
  );
}
