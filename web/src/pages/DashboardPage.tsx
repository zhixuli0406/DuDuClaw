import { useEffect, useState } from 'react';
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
import { Page, PageHeader, Card, StatCard, Badge, SkeletonList } from '@/components/ui';

// ── Task Board mini-preview (4-column Kanban summary) ────────

const PREVIEW_COLUMNS: ReadonlyArray<{
  status: TaskStatus;
  icon: React.ComponentType<{ className?: string }>;
  accent: string;
}> = [
  { status: 'todo', icon: Clock, accent: 'border-t-stone-400' },
  { status: 'in_progress', icon: AlertCircle, accent: 'border-t-amber-500' },
  { status: 'done', icon: CheckCircle2, accent: 'border-t-emerald-500' },
  { status: 'blocked', icon: Ban, accent: 'border-t-rose-500' },
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
    <Card
      title={
        <span className="flex items-center gap-2">
          <KanbanSquare className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'tasks.preview.title' })}
        </span>
      }
      actions={
        <Link
          to="/tasks"
          className="flex items-center gap-1 text-xs text-stone-500 transition-colors hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
        >
          {intl.formatMessage({ id: 'tasks.preview.viewAll' })}
          <ExternalLink className="h-3 w-3" />
        </Link>
      }
    >
      {error && !loading && (
        <div className="mb-3 rounded-md bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
          {error}
        </div>
      )}

      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {PREVIEW_COLUMNS.map(({ status, icon: Icon, accent }) => {
          const colTasks = byStatus(status).slice(0, 3);
          return (
            <div
              key={status}
              className={cn(
                'rounded-lg border-t-2 bg-stone-500/5 p-3 dark:bg-white/5',
                accent
              )}
            >
              <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-stone-600 dark:text-stone-300">
                <Icon className="h-3.5 w-3.5" />
                <span>{intl.formatMessage({ id: `tasks.column.${status}` })}</span>
                <span className="ml-auto rounded-full bg-stone-500/15 px-1.5 py-0.5 text-[10px] tabular-nums text-stone-600 dark:text-stone-300">
                  {byStatus(status).length}
                </span>
              </div>
              <div className="space-y-1.5">
                {notYetLoaded ? (
                  <SkeletonList rows={2} rowClassName="h-6" />
                ) : (
                  <>
                    {colTasks.map((t) => (
                      <div
                        key={t.id}
                        className="truncate rounded border border-[var(--panel-border)] bg-[var(--panel-fill)] px-2 py-1 text-xs text-stone-700 dark:text-stone-300"
                        title={t.title}
                      >
                        {t.title}
                      </div>
                    ))}
                    {colTasks.length === 0 && (
                      <div className="py-1 text-center text-[10px] text-stone-400 dark:text-stone-600">
                        —
                      </div>
                    )}
                  </>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {isEmpty && !loading && !error && (
        <p className="mt-3 text-center text-xs text-stone-400 dark:text-stone-600">
          {intl.formatMessage({ id: 'tasks.preview.empty' })}
        </p>
      )}
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
    <Page>
      <PageHeader
        icon={LayoutDashboard}
        title={intl.formatMessage({ id: 'nav.dashboard' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
      />

      {/* KPI tiles */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={Bot}
          tone="accent"
          label={intl.formatMessage({ id: 'dashboard.agents.title' })}
          value={agents.length}
          hint={intl.formatMessage({ id: 'dashboard.agents.active' }, { count: activeAgents })}
        />
        <StatCard
          icon={Radio}
          tone="success"
          label={intl.formatMessage({ id: 'dashboard.channels.title' })}
          value={status?.channels_connected ?? 0}
          hint={intl.formatMessage(
            { id: 'dashboard.channels.connected' },
            { count: status?.channels_connected ?? 0 }
          )}
        />
        <StatCard
          icon={Wallet}
          tone="warning"
          label={intl.formatMessage({ id: 'dashboard.budget.title' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
          hint={`/ $${(totalBudget / 100).toFixed(2)}`}
        />
        <StatCard
          icon={HeartPulse}
          tone={summary.fail ? 'danger' : summary.warn ? 'warning' : 'success'}
          label={intl.formatMessage({ id: 'dashboard.health.title' })}
          value={healthValue}
          hint={healthSubtitle}
        />
      </div>

      {/* Doctor checks */}
      {checks.length > 0 && (
        <Card title={intl.formatMessage({ id: 'dashboard.health.title' })} padded={false}>
          <div className="divide-y divide-[var(--panel-border)]">
            {checks.map((check) => (
              <div key={check.name} className="flex items-center justify-between gap-3 px-5 py-2.5">
                <span className="font-mono text-xs text-stone-700 dark:text-stone-300">
                  {check.name}
                </span>
                <div className="flex items-center gap-2.5">
                  <span className="truncate text-xs text-stone-500 dark:text-stone-400">
                    {check.message}
                  </span>
                  <span
                    className={cn(
                      'inline-block h-2 w-2 shrink-0 rounded-full',
                      check.status === 'pass'
                        ? 'bg-emerald-500'
                        : check.status === 'warn'
                          ? 'bg-amber-500'
                          : 'bg-rose-500'
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
          <Card
            className="lg:col-span-2"
            padded={false}
            title={
              <span className="flex items-center gap-2">
                <BookOpen className="h-4 w-4 text-amber-500" />
                {intl.formatMessage({ id: 'dashboard.wiki.graph' })}
              </span>
            }
            actions={
              <Link
                to="/wiki"
                className="flex items-center gap-1 text-xs text-amber-600 hover:text-amber-700 dark:text-amber-400"
              >
                {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                <ExternalLink className="h-3 w-3" />
              </Link>
            }
          >
            <WikiGraph pages={wikiPages} pageContents={wikiContents} width={650} height={350} />
          </Card>

          <Card
            title={
              <span className="flex items-center gap-2">
                <FileText className="h-4 w-4 text-amber-500" />
                {intl.formatMessage({ id: 'dashboard.wiki.recentPages' })}
              </span>
            }
            actions={<Badge tone="accent">{wikiStats?.total_pages ?? 0}</Badge>}
          >
            {wikiStats?.by_directory && (
              <div className="mb-4 flex flex-wrap gap-2">
                {Object.entries(wikiStats.by_directory).map(([dir, count]) => (
                  <Badge key={dir} tone="neutral">
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
                  className="flex items-center justify-between rounded-lg px-3 py-2 text-sm transition-colors hover:bg-stone-500/8 dark:hover:bg-white/5"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <FileText className="h-3.5 w-3.5 shrink-0 text-stone-400" />
                    <span className="truncate text-stone-700 dark:text-stone-300">{page.title}</span>
                  </div>
                  <span className="ml-2 flex shrink-0 items-center gap-1 text-xs text-stone-400">
                    <Clock className="h-3 w-3" />
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
                className="mt-3 flex items-center justify-center gap-1 rounded-lg bg-stone-500/8 py-2 text-xs text-stone-500 transition-colors hover:bg-stone-500/15 dark:bg-white/5 dark:text-stone-400"
              >
                {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                <ExternalLink className="h-3 w-3" />
              </Link>
            )}
          </Card>
        </div>
      )}

      {/* Task board preview (team Kanban overview) */}
      <TasksPreviewCard />

      {/* Activity feed */}
      <Card title={intl.formatMessage({ id: 'activity.title' })}>
        <ActivityFeed limit={10} showFilter agents={agents} />
      </Card>
    </Page>
  );
}
