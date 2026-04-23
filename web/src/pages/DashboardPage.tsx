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
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';

function StatCard({
  icon: Icon,
  title,
  value,
  subtitle,
  color,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  value: string | number;
  subtitle: string;
  color: string;
}) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3">
        <div className={cn('rounded-lg p-2.5', color)}>
          <Icon className="h-5 w-5 text-white" />
        </div>
        <div>
          <p className="text-sm text-stone-500 dark:text-stone-400">{title}</p>
          <p className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
            {value}
          </p>
          <p className="text-xs text-stone-400 dark:text-stone-500">
            {subtitle}
          </p>
        </div>
      </div>
    </div>
  );
}

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

  const byStatus = (status: TaskStatus) =>
    tasks.filter((t) => t.status === status);
  const isEmpty = tasks.length === 0;
  // Distinguish "never loaded yet" from "loaded empty" to avoid
  // rendering a confusing empty state before the first fetch returns.
  const notYetLoaded = loading && isEmpty;

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <KanbanSquare className="h-5 w-5 text-amber-500" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'tasks.preview.title' })}
          </h3>
        </div>
        <Link
          to="/tasks"
          className="flex items-center gap-1 text-xs text-stone-500 transition-colors hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
        >
          {intl.formatMessage({ id: 'tasks.preview.viewAll' })}
          <ExternalLink className="h-3 w-3" />
        </Link>
      </div>

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
                'rounded-lg border-t-2 bg-stone-50 p-3 dark:bg-stone-800/40',
                accent,
              )}
            >
              <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-stone-600 dark:text-stone-300">
                <Icon className="h-3.5 w-3.5" />
                <span>
                  {intl.formatMessage({ id: `tasks.column.${status}` })}
                </span>
                <span className="ml-auto rounded-full bg-stone-200 px-1.5 py-0.5 text-[10px] text-stone-600 dark:bg-stone-700 dark:text-stone-300">
                  {byStatus(status).length}
                </span>
              </div>
              <div className="space-y-1.5">
                {notYetLoaded ? (
                  // Skeleton cards on first paint (no flash of empty state)
                  <>
                    <div className="h-6 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
                    <div className="h-6 animate-pulse rounded bg-stone-200 dark:bg-stone-700" />
                  </>
                ) : (
                  <>
                    {colTasks.map((t) => (
                      <div
                        key={t.id}
                        className="truncate rounded border border-stone-200 bg-white px-2 py-1 text-xs text-stone-700 dark:border-stone-700 dark:bg-stone-900 dark:text-stone-300"
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
    </div>
  );
}

export function DashboardPage() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const { status, fetchStatus } = useSystemStore();
  const connectionState = useConnectionStore((s) => s.state);
  const [budget, setBudget] = useState<BudgetSummary | null>(null);
  const [doctor, setDoctor] = useState<{ checks: DoctorCheck[]; summary: { pass: number; warn: number; fail: number } } | null>(null);
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
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    api.system.doctor().then(setDoctor).catch((e) => {
      console.warn("[api]", e);
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
          await Promise.all(batch.map(async (p) => {
            try {
              const r = await api.wiki.read(mainAgent, p.path);
              contents[p.path] = r?.content ?? '';
            } catch { /* skip */ }
          }));
        }
        setWikiContents(contents);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });

    // Lightweight budget refresh every 60s.
    // Silent on failure to avoid spamming toasts from a transient blip — the
    // initial load already surfaces errors, and the WS "disconnected" indicator
    // covers the outage case.
    const interval = setInterval(() => {
      api.accounts.budgetSummary().then(setBudget).catch((e) => console.warn("[api]", e));
    }, 60_000);
    return () => clearInterval(interval);
  }, [connectionState, fetchAgents, fetchStatus]);

  const activeAgents = agents.filter((a) => a.status === 'active').length;
  const totalBudget = budget?.total_budget_cents ?? 0;
  const totalSpent = budget?.total_spent_cents ?? 0;

  const checks = doctor?.checks ?? [];
  const summary = doctor?.summary ?? { pass: 0, warn: 0, fail: 0 };
  const healthValue = doctor
    ? `${summary.pass}/${checks.length}`
    : '—';
  const healthSubtitle = doctor
    ? summary.fail > 0
      ? intl.formatMessage({ id: 'dashboard.health.failCount' }, { count: summary.fail })
      : summary.warn > 0
        ? intl.formatMessage({ id: 'dashboard.health.warnCount' }, { count: summary.warn })
        : intl.formatMessage({ id: 'dashboard.health.allPassed' })
    : intl.formatMessage({ id: 'common.loading' });

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'nav.dashboard' })}
      </h2>

      {/* Stat Cards */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={Bot}
          title={intl.formatMessage({ id: 'dashboard.agents.title' })}
          value={agents.length}
          subtitle={intl.formatMessage({ id: 'dashboard.agents.active' }, { count: activeAgents })}
          color="bg-amber-500"
        />
        <StatCard
          icon={Radio}
          title={intl.formatMessage({ id: 'dashboard.channels.title' })}
          value={status?.channels_connected ?? 0}
          subtitle={intl.formatMessage({ id: 'dashboard.channels.connected' }, { count: status?.channels_connected ?? 0 })}
          color="bg-emerald-500"
        />
        <StatCard
          icon={Wallet}
          title={intl.formatMessage({ id: 'dashboard.budget.title' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
          subtitle={`/ $${(totalBudget / 100).toFixed(2)}`}
          color="bg-orange-400"
        />
        <StatCard
          icon={HeartPulse}
          title={intl.formatMessage({ id: 'dashboard.health.title' })}
          value={healthValue}
          subtitle={healthSubtitle}
          color={summary.fail ? 'bg-rose-500' : summary.warn ? 'bg-amber-500' : 'bg-emerald-500'}
        />
      </div>

      {/* Doctor Checks */}
      {checks.length > 0 && (
        <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
          <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'dashboard.health.title' })}
          </h3>
          <div className="space-y-2">
            {checks.map((check) => (
              <div key={check.name} className="flex items-center justify-between rounded-lg bg-stone-50 px-4 py-2.5 dark:bg-stone-800/50">
                <span className="text-sm text-stone-700 dark:text-stone-300">{check.name}</span>
                <div className="flex items-center gap-2">
                  <span className="text-xs text-stone-500 dark:text-stone-400">{check.message}</span>
                  <span className={cn(
                    'inline-block h-2 w-2 rounded-full',
                    check.status === 'pass' ? 'bg-emerald-500' : check.status === 'warn' ? 'bg-amber-500' : 'bg-rose-500'
                  )} />
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Wiki Knowledge Graph + Recent Pages */}
      {wikiStats?.exists && wikiPages.length > 0 && (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
          {/* Knowledge Graph */}
          <div className="lg:col-span-2 rounded-xl border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900 overflow-hidden">
            <div className="flex items-center justify-between border-b border-stone-200 px-5 py-3 dark:border-stone-800">
              <h3 className="flex items-center gap-2 text-lg font-medium text-stone-900 dark:text-stone-50">
                <BookOpen className="h-5 w-5 text-amber-500" />
                {intl.formatMessage({ id: 'dashboard.wiki.graph' })}
              </h3>
              <Link
                to="/wiki"
                className="flex items-center gap-1 text-xs text-amber-600 hover:text-amber-700 dark:text-amber-400"
              >
                {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                <ExternalLink className="h-3 w-3" />
              </Link>
            </div>
            <WikiGraph
              pages={wikiPages}
              pageContents={wikiContents}
              width={650}
              height={350}
            />
          </div>

          {/* Recent Wiki Pages */}
          <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
            <div className="flex items-center justify-between mb-4">
              <h3 className="flex items-center gap-2 text-lg font-medium text-stone-900 dark:text-stone-50">
                <FileText className="h-5 w-5 text-amber-500" />
                {intl.formatMessage({ id: 'dashboard.wiki.recentPages' })}
              </h3>
              <span className="rounded-full bg-amber-100 px-2 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                {wikiStats?.total_pages ?? 0}
              </span>
            </div>

            {/* Stats summary */}
            {wikiStats?.by_directory && (
              <div className="mb-4 flex flex-wrap gap-2">
                {Object.entries(wikiStats.by_directory).map(([dir, count]) => (
                  <span key={dir} className="inline-flex items-center gap-1 rounded-full bg-stone-100 px-2.5 py-1 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                    {dir}/ <span className="font-medium">{count}</span>
                  </span>
                ))}
              </div>
            )}

            {/* Recent pages list */}
            <div className="space-y-2">
              {wikiPages.slice(0, 8).map((page) => (
                <Link
                  key={page.path}
                  to="/wiki"
                  className="flex items-center justify-between rounded-lg px-3 py-2 text-sm transition-colors hover:bg-stone-50 dark:hover:bg-stone-800"
                >
                  <div className="flex items-center gap-2 min-w-0">
                    <FileText className="h-3.5 w-3.5 shrink-0 text-stone-400" />
                    <span className="truncate text-stone-700 dark:text-stone-300">
                      {page.title}
                    </span>
                  </div>
                  <span className="ml-2 shrink-0 flex items-center gap-1 text-xs text-stone-400">
                    <Clock className="h-3 w-3" />
                    {new Date(page.updated).toLocaleDateString('zh-TW', { month: 'short', day: 'numeric' })}
                  </span>
                </Link>
              ))}
            </div>

            {wikiPages.length > 8 && (
              <Link
                to="/wiki"
                className="mt-3 flex items-center justify-center gap-1 rounded-lg bg-stone-50 py-2 text-xs text-stone-500 transition-colors hover:bg-stone-100 dark:bg-stone-800 dark:text-stone-400 dark:hover:bg-stone-700"
              >
                {intl.formatMessage({ id: 'dashboard.wiki.viewAll' })}
                <ExternalLink className="h-3 w-3" />
              </Link>
            )}
          </div>
        </div>
      )}

      {/* Task Board Preview (Multica-style team Kanban overview) */}
      <TasksPreviewCard />

      {/* Activity Feed */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'activity.title' })}
        </h3>
        <ActivityFeed limit={10} showFilter agents={agents} />
      </div>
    </div>
  );
}
