import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { api, type TaskInfo, type DashboardLayoutWidget } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useVisibleAgents } from '@/lib/data-scope';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import { Page, Card, Button } from '@/components/ui';
import { ActivityFeed } from '@/components/ActivityFeed';
import {
  type InboxItem,
  sortInbox,
  TYPE_URGENCY,
} from '@/lib/inbox-model';
import { GreetingHud } from '@/components/home/GreetingHud';
import { WorldStagePlaceholder } from '@/components/home/WorldStagePlaceholder';
import { NeedsMeRow } from '@/components/home/NeedsMeRow';
import { LiveCards } from '@/components/home/LiveCards';
import { RecentTasks } from '@/components/home/RecentTasks';
import { ChannelHealthCard } from '@/components/home/ChannelHealthCard';
import { LayoutGrid, ArrowUp, ArrowDown, EyeOff as EyeOffIcon, Plus, Check, X, Eye } from 'lucide-react';
import { hasMinRole } from '@/lib/roles';

/** Same calendar day in local time. */
function isToday(iso?: string | null): boolean {
  if (!iso) return false;
  const t = new Date(iso);
  const now = new Date();
  return t.getFullYear() === now.getFullYear() && t.getMonth() === now.getMonth() && t.getDate() === now.getDate();
}

/** Default widget order for a fresh user (server catalog order also matches). */
const DEFAULT_ORDER = ['needs_me', 'my_agents', 'recent_activity', 'my_tasks', 'channel_health'];
/** Widgets that span the full row; the rest pack into a two-column grid. */
const FULL_SPAN = new Set(['needs_me', 'my_agents']);

/**
 * HomePage (`/`) — 首頁「事務所」 (dashboard-redesign-v2 §5.1) + WP15 personal
 * dashboard: the war-report HUD and world band stay fixed; everything below is
 * a per-user widget list (server-persisted order + visibility, catalog is
 * role-filtered fail-closed by the gateway).
 */
export function HomePage() {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  // Data-scoped: an employee sees only their own AI staff (§3.4 WP11-T11.3).
  const agents = useVisibleAgents();
  const connectionState = useConnectionStore((s) => s.state);
  const authed = connectionState === 'authenticated';

  const [needsMe, setNeedsMe] = useState<InboxItem[]>([]);
  const [spentCents, setSpentCents] = useState<number | null>(null);
  const [doneToday, setDoneToday] = useState<number | null>(null);

  // ── WP15 layout state ──
  const [catalog, setCatalog] = useState<string[]>([]);
  const [layout, setLayout] = useState<DashboardLayoutWidget[]>([]);
  const [editing, setEditing] = useState(false);
  const [savedLayout, setSavedLayout] = useState<DashboardLayoutWidget[]>([]);

  // ── Read-only subordinate view (view-as, manager+). `?view_as=<user_id>`
  // renders THAT user's layout + data scope; editing is disabled and there is
  // no write path for someone else's layout (structural, not just hidden UI).
  const [searchParams, setSearchParams] = useSearchParams();
  const viewAs = searchParams.get('view_as') ?? '';
  const [viewTarget, setViewTarget] = useState<{
    name: string;
    role: string;
    layout: DashboardLayoutWidget[];
    boundAgents: string[];
  } | null>(null);
  const [subordinates, setSubordinates] = useState<Array<{ id: string; display_name: string }>>([]);
  const isManagerUp = hasMinRole(user?.role, 'manager');

  useEffect(() => {
    if (!authed || !viewAs) {
      setViewTarget(null);
      return;
    }
    let alive = true;
    api.dashboard
      .layoutView(viewAs)
      .then((r) => {
        if (!alive) return;
        const ids = r.widgets.map((w) => w.id);
        const savedWidgets = (r.layout?.widgets ?? []).filter((w) => ids.includes(w.id));
        const known = new Set(savedWidgets.map((w) => w.id));
        const ordered = savedWidgets.length > 0
          ? [...savedWidgets, ...ids.filter((id) => !known.has(id)).map((id) => ({ id, hidden: false }))]
          : DEFAULT_ORDER.filter((id) => ids.includes(id)).map((id) => ({ id, hidden: false }));
        setViewTarget({
          name: r.user.display_name,
          role: r.user.role,
          layout: ordered,
          boundAgents: r.bound_agents,
        });
      })
      .catch((e) => {
        toast.error(formatError(e));
        setSearchParams({}, { replace: true });
      });
    return () => {
      alive = false;
    };
  }, [authed, viewAs, setSearchParams]);

  // Picker options — lazy, manager+ only, harmless if it fails.
  useEffect(() => {
    if (!authed || !isManagerUp) return;
    let alive = true;
    api.users
      .subordinates()
      .then((r) => alive && setSubordinates(r.users))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [authed, isManagerUp]);

  useEffect(() => {
    if (!authed) return;
    let alive = true;
    Promise.all([
      api.dashboard.widgetsCatalog().catch(() => ({ widgets: [] as Array<{ id: string }> })),
      api.dashboard.layoutGet().catch(() => ({ layout: null })),
    ]).then(([cat, saved]) => {
      if (!alive) return;
      const ids = cat.widgets.map((w) => w.id);
      setCatalog(ids);
      // Effective layout = saved order ∩ catalog, then any new catalog widgets
      // appended visible — a widget added in an upgrade shows up by itself.
      const savedWidgets = (saved.layout?.widgets ?? []).filter((w) => ids.includes(w.id));
      const known = new Set(savedWidgets.map((w) => w.id));
      const ordered = savedWidgets.length > 0
        ? [...savedWidgets, ...ids.filter((id) => !known.has(id)).map((id) => ({ id, hidden: false }))]
        : DEFAULT_ORDER.filter((id) => ids.includes(id)).map((id) => ({ id, hidden: false }));
      setLayout(ordered);
      setSavedLayout(ordered);
    });
    return () => {
      alive = false;
    };
  }, [authed]);

  useEffect(() => {
    if (!authed) return;
    fetchAgents();

    const nameOf = (id: string) => {
      const a = agents.find((x) => x.name === id);
      return a?.display_name || id;
    };

    // "需要我" merged stream — four cheap sources (approvals / blocked / budget /
    // failed run). Each is best-effort: a manager-gated source that errors for
    // this viewer contributes nothing (fail-safe, not fail-loud). Per-agent
    // decisions are intentionally omitted here — they cost N calls and belong to
    // the full /inbox, not a home preview.
    Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      api.audit.unifiedLog({ sources: ['channel_failure'], limit: 20 }).catch(() => null),
    ]).then(([approvals, budget, blocked, failed]) => {
      const items: InboxItem[] = [];
      for (const a of approvals?.approvals ?? []) {
        items.push({ id: `approval:${a.id}`, type: 'approval', title: a.summary, agentId: a.agent_id, timestamp: a.created_at, urgency: TYPE_URGENCY.approval, actionable: true, status: 'pending' });
      }
      for (const t of blocked?.tasks ?? []) {
        items.push({ id: `blocked:${t.id}`, type: 'blocked', title: t.title, agentId: t.assigned_to || undefined, timestamp: t.updated_at, urgency: TYPE_URGENCY.blocked, actionable: true, status: t.status });
      }
      for (const inc of budget?.incidents ?? []) {
        items.push({ id: `budget:${inc.agent_id}:${inc.ts}`, type: 'budget', title: intl.formatMessage({ id: 'inbox.budget.title' }, { agent: nameOf(inc.agent_id), scope: inc.scope }), agentId: inc.agent_id, timestamp: inc.ts, urgency: TYPE_URGENCY.budget, actionable: true, status: inc.event });
      }
      for (const ev of failed?.events ?? []) {
        const ch = typeof ev.details?.channel === 'string' ? (ev.details.channel as string) : undefined;
        items.push({ id: `failed_run:${ev.agent_id}:${ev.timestamp}`, type: 'failed_run', title: ev.summary || intl.formatMessage({ id: 'inbox.failedRun.title' }, { agent: nameOf(ev.agent_id) }), agentId: ev.agent_id || undefined, channel: ch, timestamp: ev.timestamp, urgency: TYPE_URGENCY.failed_run, actionable: false, status: ev.severity });
      }
      setNeedsMe(items);
    }).catch(() => { /* silent — an empty strip is honest */ });

    // Cost tile: no per-day spend on the wired RPC surface, so show the
    // cumulative total (labelled 「累計」 in the HUD) rather than fake a today value.
    api.accounts.budgetSummary()
      .then((b) => setSpentCents(b?.total_spent_cents ?? 0))
      .catch(() => setSpentCents(null));

    api.tasks.list({ status: 'done' })
      .then((r) => setDoneToday((r?.tasks ?? []).filter((t: TaskInfo) => isToday(t.completed_at)).length))
      .catch(() => setDoneToday(null));
    // agents intentionally excluded from deps: the name map is a display nicety
    // resolved at fetch time; re-running on every agent tick would spam the RPCs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authed, fetchAgents, intl]);

  const busyCount = useMemo(() => agents.filter((a) => a.status === 'active').length, [agents]);
  const actionableCount = useMemo(() => needsMe.filter((i) => i.actionable).length, [needsMe]);
  const previewTop = useMemo(() => sortInbox(needsMe, 'urgency').slice(0, 3), [needsMe]);

  // ── WP15 edit actions ──
  const move = (id: string, dir: -1 | 1) => {
    setLayout((cur) => {
      const i = cur.findIndex((w) => w.id === id);
      const j = i + dir;
      if (i < 0 || j < 0 || j >= cur.length) return cur;
      const next = [...cur];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  };
  const setHidden = (id: string, hidden: boolean) =>
    setLayout((cur) => cur.map((w) => (w.id === id ? { ...w, hidden } : w)));

  const saveLayout = useCallback(async () => {
    try {
      await api.dashboard.layoutSet(layout);
      setSavedLayout(layout);
      setEditing(false);
      toast.success(intl.formatMessage({ id: 'home.layout.saved' }));
    } catch (e) {
      toast.error(formatError(e));
    }
  }, [intl, layout]);

  const renderWidget = (id: string): ReactNode => {
    switch (id) {
      case 'needs_me':
        return <NeedsMeRow items={previewTop} total={needsMe.length} />;
      case 'my_agents':
        return <LiveCards agents={scopedAgents} enabled={authed} />;
      case 'recent_activity':
        return (
          <Card title={intl.formatMessage({ id: 'activity.title' })}>
            <ActivityFeed limit={10} showFilter agents={scopedAgents} />
          </Card>
        );
      case 'my_tasks':
        return <RecentTasks agents={scopedAgents} enabled={authed} />;
      case 'channel_health':
        return <ChannelHealthCard enabled={authed} />;
      default:
        return null; // unknown id from a newer server — skip silently
    }
  };

  const viewing = viewAs !== '' && viewTarget !== null;
  // In view-as mode, scope widget data the way the target sees it (WP11):
  // an employee sees only their bound agents; manager+ targets see all.
  const scopedAgents = useMemo(() => {
    if (!viewing || !viewTarget) return agents;
    if (viewTarget.role !== 'employee') return agents;
    const bound = new Set(viewTarget.boundAgents);
    return agents.filter((a) => bound.has(a.name));
  }, [agents, viewing, viewTarget]);

  const activeLayout = viewing && viewTarget ? viewTarget.layout : layout;
  const visible = activeLayout.filter((w) => !w.hidden);
  const hiddenWidgets = layout.filter((w) => w.hidden);

  return (
    <Page wide>
      {/* T3.1 — greeting HUD + today's war-report (fixed, not a widget) */}
      <GreetingHud
        userName={user?.display_name || intl.formatMessage({ id: 'home.greeting.fallbackName' })}
        busyCount={busyCount}
        totalAgents={agents.length}
        actionableCount={actionableCount}
        doneToday={doneToday}
        costCents={spentCents}
      />

      {/* T3.5 — world stage mount (fixed) */}
      <WorldStagePlaceholder agents={agents} />

      {/* Read-only view-as banner (§view-as: look, never touch). */}
      {viewing && viewTarget && (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-amber-300/60 bg-amber-500/10 px-4 py-2.5 dark:border-amber-500/40">
          <span className="flex items-center gap-2 text-sm text-amber-800 dark:text-amber-300">
            <Eye className="h-4 w-4 shrink-0" />
            {intl.formatMessage({ id: 'home.viewAs.banner' }, { name: viewTarget.name })}
          </span>
          <Button variant="secondary" icon={X} onClick={() => setSearchParams({}, { replace: true })}>
            {intl.formatMessage({ id: 'home.viewAs.exit' })}
          </Button>
        </div>
      )}

      {/* WP15 — edit-mode toolbar. Zero visual noise when not editing. */}
      {!viewing && (
        <div className="flex items-center justify-end gap-3">
          {editing ? (
            <div className="flex items-center gap-2">
              <Button variant="ghost" icon={X} onClick={() => { setLayout(savedLayout); setEditing(false); }}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button variant="primary" icon={Check} onClick={() => void saveLayout()}>
                {intl.formatMessage({ id: 'home.layout.done' })}
              </Button>
            </div>
          ) : (
            <>
              {isManagerUp && subordinates.length > 0 && (
                <label className="inline-flex items-center gap-1.5 text-xs text-stone-400 dark:text-stone-500">
                  <Eye className="h-3.5 w-3.5" />
                  <select
                    value=""
                    onChange={(e) => {
                      if (e.target.value) setSearchParams({ view_as: e.target.value });
                    }}
                    className="rounded border border-transparent bg-transparent py-0.5 text-xs text-stone-400 transition-colors hover:text-stone-600 focus:outline-none dark:text-stone-500 dark:hover:text-stone-300"
                  >
                    <option value="">{intl.formatMessage({ id: 'home.viewAs.pick' })}</option>
                    {subordinates.map((s) => (
                      <option key={s.id} value={s.id}>{s.display_name}</option>
                    ))}
                  </select>
                </label>
              )}
              <button
                onClick={() => setEditing(true)}
                className="inline-flex items-center gap-1.5 rounded px-2 py-1 text-xs text-stone-400 transition-colors hover:text-stone-600 dark:text-stone-500 dark:hover:text-stone-300"
              >
                <LayoutGrid className="h-3.5 w-3.5" />
                {intl.formatMessage({ id: 'home.layout.edit' })}
              </button>
            </>
          )}
        </div>
      )}

      {/* Widget list — full-span widgets break the two-column flow. */}
      <div className="grid gap-6 lg:grid-cols-2">
        {visible.map((w, idx) => (
          <div
            key={w.id}
            className={cn(
              FULL_SPAN.has(w.id) && 'lg:col-span-2',
              editing && 'relative rounded-xl ring-2 ring-amber-400/60 ring-offset-2 ring-offset-[var(--page-bg,transparent)]',
            )}
          >
            {editing && (
              <div className="absolute -top-3 right-3 z-10 flex items-center gap-1 rounded-full border border-[var(--panel-border)] bg-[var(--panel-bg,#fff)] px-1.5 py-0.5 shadow-sm dark:bg-stone-800">
                <button onClick={() => move(w.id, -1)} disabled={idx === 0} className="rounded p-0.5 text-stone-500 hover:text-stone-800 disabled:opacity-30 dark:hover:text-stone-200" aria-label="move up">
                  <ArrowUp className="h-3.5 w-3.5" />
                </button>
                <button onClick={() => move(w.id, 1)} disabled={idx === visible.length - 1} className="rounded p-0.5 text-stone-500 hover:text-stone-800 disabled:opacity-30 dark:hover:text-stone-200" aria-label="move down">
                  <ArrowDown className="h-3.5 w-3.5" />
                </button>
                <button onClick={() => setHidden(w.id, true)} className="rounded p-0.5 text-rose-500 hover:text-rose-700" aria-label="hide">
                  <EyeOffIcon className="h-3.5 w-3.5" />
                </button>
              </div>
            )}
            {renderWidget(w.id)}
          </div>
        ))}
      </div>

      {/* Edit mode: hidden / addable widgets drawer. */}
      {editing && (
        <Card title={intl.formatMessage({ id: 'home.layout.hiddenTitle' })}>
          {hiddenWidgets.length === 0 ? (
            <p className="py-2 text-center text-xs text-stone-400">{intl.formatMessage({ id: 'home.layout.hiddenEmpty' })}</p>
          ) : (
            <div className="flex flex-wrap gap-2">
              {hiddenWidgets.map((w) => (
                <button
                  key={w.id}
                  onClick={() => setHidden(w.id, false)}
                  className="inline-flex items-center gap-1.5 rounded-full border border-amber-300 px-3 py-1 text-xs font-medium text-amber-700 hover:bg-amber-500/10 dark:border-amber-500/40 dark:text-amber-400"
                >
                  <Plus className="h-3 w-3" />
                  {intl.formatMessage({ id: `home.widget.${w.id}`, defaultMessage: w.id })}
                </button>
              ))}
            </div>
          )}
          {catalog.length === 0 && (
            <p className="mt-2 text-xs text-stone-400">{intl.formatMessage({ id: 'home.layout.catalogUnavailable' })}</p>
          )}
        </Card>
      )}
    </Page>
  );
}
