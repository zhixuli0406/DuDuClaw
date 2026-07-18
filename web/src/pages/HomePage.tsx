import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import { api, type DashboardLayoutWidget, type CustomWidgetSummary } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useVisibleAgents } from '@/lib/data-scope';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  CardHeader,
  CardTitle,
  Button,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from '@/components/mds';
import { ActivityFeed } from '@/components/ActivityFeed';
import {
  type InboxItem,
  sortInbox,
  TYPE_URGENCY,
} from '@/lib/inbox-model';
import { NeedsMeRow } from '@/components/home/NeedsMeRow';
import { LiveCards } from '@/components/home/LiveCards';
import { RecentTasks } from '@/components/home/RecentTasks';
import { ChannelHealthCard } from '@/components/home/ChannelHealthCard';
import { UsageSummary } from '@/components/home/UsageSummary';
import { CustomWidgetFrame } from '@/components/home/CustomWidgetFrame';
import {
  LayoutGrid,
  ArrowUp,
  ArrowDown,
  EyeOff as EyeOffIcon,
  GripVertical,
  MoreVertical,
  Pencil,
  Plus,
  Check,
  X,
  Eye,
} from 'lucide-react';
import { hasMinRole } from '@/lib/roles';

/** Default widget order for a fresh user (server catalog order also matches). */
const DEFAULT_ORDER = ['needs_me', 'my_agents', 'recent_activity', 'my_tasks', 'channel_health'];
/** Widgets that span the full row; the rest pack into a two-column grid. */
const FULL_SPAN = new Set(['needs_me', 'my_agents']);

/** Local-time hour → greeting bucket. */
function greetingBucket(hour: number): 'morning' | 'afternoon' | 'evening' | 'night' {
  if (hour >= 5 && hour < 12) return 'morning';
  if (hour >= 12 && hour < 18) return 'afternoon';
  if (hour >= 18 && hour < 23) return 'evening';
  return 'night';
}

/**
 * HomePage (`/`) — Multica-style work overview (WP1.5). A reading-type container
 * (§5.2): a plain greeting line, the fixed 用量摘要 KPI strip (§5.5), then the
 * per-user widget list (server-persisted order + visibility, catalog role-filtered
 * fail-closed by the gateway). The PixiJS world stage lives on its own `/world`
 * page now — the home canvas stays calm and report-shaped.
 */
export function HomePage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const user = useAuthStore((s) => s.user);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  // Data-scoped: an employee sees only their own AI staff (§3.4 WP11-T11.3).
  const agents = useVisibleAgents();
  const connectionState = useConnectionStore((s) => s.state);
  const authed = connectionState === 'authenticated';

  const [needsMe, setNeedsMe] = useState<InboxItem[]>([]);

  // ── widget layout state ──
  const [catalog, setCatalog] = useState<string[]>([]);
  const [layout, setLayout] = useState<DashboardLayoutWidget[]>([]);
  const [editing, setEditing] = useState(false);
  const [savedLayout, setSavedLayout] = useState<DashboardLayoutWidget[]>([]);
  // Custom widgets visible to me (mine + instance-shared) — layout entries
  // reference them as `custom:<id>` and render in a sandboxed iframe.
  const [customWidgets, setCustomWidgets] = useState<CustomWidgetSummary[]>([]);

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
    customWidgets: Array<{ id: string; title: string; html: string }>;
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
        const customs = r.custom_widgets ?? [];
        const customIds = new Set(customs.map((w) => `custom:${w.id}`));
        const savedWidgets = (r.layout?.widgets ?? []).filter(
          (w) => ids.includes(w.id) || customIds.has(w.id),
        );
        const known = new Set(savedWidgets.map((w) => w.id));
        const ordered = savedWidgets.length > 0
          ? [...savedWidgets, ...ids.filter((id) => !known.has(id)).map((id) => ({ id, hidden: false }))]
          : DEFAULT_ORDER.filter((id) => ids.includes(id)).map((id) => ({ id, hidden: false }));
        setViewTarget({
          name: r.user.display_name,
          role: r.user.role,
          layout: ordered,
          boundAgents: r.bound_agents,
          customWidgets: customs,
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
      api.widgetsCustom.list().catch(() => ({ widgets: [] as CustomWidgetSummary[] })),
    ]).then(([cat, saved, custom]) => {
      if (!alive) return;
      const ids = cat.widgets.map((w) => w.id);
      setCatalog(ids);
      setCustomWidgets(custom.widgets);
      // Effective layout = saved order ∩ (catalog ∪ my visible custom
      // widgets), then any new catalog widgets appended visible — a widget
      // added in an upgrade shows up by itself. Custom widgets are only ever
      // added explicitly from the edit drawer.
      const customIds = new Set(custom.widgets.map((w) => `custom:${w.id}`));
      const savedWidgets = (saved.layout?.widgets ?? []).filter(
        (w) => ids.includes(w.id) || customIds.has(w.id),
      );
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
    // this viewer contributes nothing (fail-safe, not fail-loud).
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
    // agents intentionally excluded from deps: the name map is a display nicety
    // resolved at fetch time; re-running on every agent tick would spam the RPCs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authed, fetchAgents, intl]);

  const previewTop = useMemo(() => sortInbox(needsMe, 'urgency').slice(0, 4), [needsMe]);

  // ── edit actions ──
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

  const viewing = viewAs !== '' && viewTarget !== null;
  // In view-as mode, scope widget data the way the target sees it (WP11).
  const scopedAgents = useMemo(() => {
    if (!viewing || !viewTarget) return agents;
    if (viewTarget.role !== 'employee') return agents;
    const bound = new Set(viewTarget.boundAgents);
    return agents.filter((a) => bound.has(a.name));
  }, [agents, viewing, viewTarget]);

  const renderWidget = (id: string): ReactNode => {
    switch (id) {
      case 'needs_me':
        return <NeedsMeRow items={previewTop} total={needsMe.length} />;
      case 'my_agents':
        return <LiveCards agents={scopedAgents} enabled={authed} />;
      case 'recent_activity':
        return (
          <Card>
            <CardHeader>
              <CardTitle>{intl.formatMessage({ id: 'activity.title' })}</CardTitle>
            </CardHeader>
            <div className="px-4">
              <ActivityFeed limit={10} showFilter agents={scopedAgents} />
            </div>
          </Card>
        );
      case 'my_tasks':
        return <RecentTasks agents={scopedAgents} enabled={authed} />;
      case 'channel_health':
        return <ChannelHealthCard enabled={authed} />;
      default: {
        // `custom:<id>` → sandboxed custom widget. In view-as mode the html
        // arrives inline with the layout; on my own board the frame lazy-loads it.
        const cid = id.startsWith('custom:') ? id.slice('custom:'.length) : null;
        if (cid) {
          const kebab = !viewing ? (
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button variant="ghost" size="icon-xs" aria-label={intl.formatMessage({ id: 'common.more' })}>
                    <MoreVertical />
                  </Button>
                }
              />
              <DropdownMenuContent>
                <DropdownMenuItem onClick={() => navigate(`/widgets/${encodeURIComponent(cid)}/edit`)}>
                  <Pencil />
                  {intl.formatMessage({ id: 'home.widget.custom.edit' })}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          ) : undefined;

          if (viewing && viewTarget) {
            const w = viewTarget.customWidgets.find((x) => x.id === cid);
            return w ? <CustomWidgetFrame html={w.html} title={w.title} /> : null;
          }
          const meta = customWidgets.find((w) => w.id === cid);
          return <CustomWidgetFrame widgetId={cid} title={meta?.title} headerAction={kebab} />;
        }
        return null; // unknown id from a newer server — skip silently
      }
    }
  };

  /** Display label for a layout id — custom widgets use their own title. */
  const widgetLabel = (id: string): string => {
    if (id.startsWith('custom:')) {
      const meta = customWidgets.find((w) => `custom:${w.id}` === id);
      return meta?.title ?? intl.formatMessage({ id: 'widgets.frame.unknown' });
    }
    return intl.formatMessage({ id: `home.widget.${id}`, defaultMessage: id });
  };

  // Custom widgets visible to me but not yet on my board — addable in edit mode.
  const layoutIds = new Set(layout.map((w) => w.id));
  const addableCustom = customWidgets.filter((w) => !layoutIds.has(`custom:${w.id}`));
  const addCustom = (id: string) =>
    setLayout((cur) => [...cur, { id: `custom:${id}`, hidden: false }]);

  const activeLayout = viewing && viewTarget ? viewTarget.layout : layout;
  const visible = activeLayout
    .filter((w) => !w.hidden)
    // 拍板: the "需要我" strip is silent when empty (except in edit mode, where
    // every widget must stay reorderable/unhideable).
    .filter((w) => !(w.id === 'needs_me' && !editing && needsMe.length === 0));
  const hiddenWidgets = layout.filter((w) => w.hidden);

  const greeting = intl.formatMessage(
    { id: `home.greeting.${greetingBucket(new Date().getHours())}` },
    { name: user?.display_name || intl.formatMessage({ id: 'home.greeting.fallbackName' }) },
  );

  return (
    <div className="mx-auto w-full max-w-6xl space-y-5">
      {/* Plain greeting line (§5.2 reading container — no HUD). */}
      <div className="min-w-0">
        <h1 className="truncate text-base font-medium text-foreground">{greeting}</h1>
        <p className="text-sm text-muted-foreground">
          {intl.formatDate(new Date(), { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })}
        </p>
      </div>

      {/* Fixed 用量摘要 KPI strip (§5.5). */}
      <UsageSummary enabled={authed} />

      {/* Read-only view-as banner (§view-as: look, never touch). */}
      {viewing && viewTarget && (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-warning/40 bg-warning/10 px-4 py-2.5">
          <span className="flex items-center gap-2 text-sm text-foreground">
            <Eye className="size-4 shrink-0 text-warning" />
            {intl.formatMessage({ id: 'home.viewAs.banner' }, { name: viewTarget.name })}
          </span>
          <Button variant="secondary" size="sm" onClick={() => setSearchParams({}, { replace: true })}>
            <X />
            {intl.formatMessage({ id: 'home.viewAs.exit' })}
          </Button>
        </div>
      )}

      {/* Edit-mode toolbar. Zero visual noise when not editing. */}
      {!viewing && (
        <div className="flex items-center justify-end gap-2">
          {editing ? (
            <>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setLayout(savedLayout);
                  setEditing(false);
                }}
              >
                <X />
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button variant="brand" size="sm" onClick={() => void saveLayout()}>
                <Check />
                {intl.formatMessage({ id: 'home.layout.done' })}
              </Button>
            </>
          ) : (
            <>
              {isManagerUp && subordinates.length > 0 && (
                <label className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                  <Eye className="size-3.5" />
                  <select
                    value=""
                    onChange={(e) => {
                      if (e.target.value) setSearchParams({ view_as: e.target.value });
                    }}
                    className="rounded-md border border-transparent bg-transparent py-0.5 text-xs text-muted-foreground transition-colors hover:text-foreground focus:outline-none"
                  >
                    <option value="">{intl.formatMessage({ id: 'home.viewAs.pick' })}</option>
                    {subordinates.map((s) => (
                      <option key={s.id} value={s.id}>{s.display_name}</option>
                    ))}
                  </select>
                </label>
              )}
              <Button variant="ghost" size="sm" onClick={() => setEditing(true)}>
                <LayoutGrid />
                {intl.formatMessage({ id: 'home.layout.edit' })}
              </Button>
            </>
          )}
        </div>
      )}

      {/* Widget list — full-span widgets break the two-column flow. */}
      <div className="grid gap-5 lg:grid-cols-2">
        {visible.map((w, idx) => (
          <div
            key={w.id}
            className={cn(
              FULL_SPAN.has(w.id) && 'lg:col-span-2',
              editing && 'relative rounded-xl ring-2 ring-brand/25',
            )}
          >
            {editing && (
              <div className="absolute -top-3 right-3 z-10 flex items-center gap-0.5 rounded-lg bg-surface-raised px-1 py-0.5 shadow-[var(--menu-shadow)] ring-1 ring-surface-border">
                <span className="grid place-items-center px-0.5 text-muted-foreground/60" aria-hidden>
                  <GripVertical className="size-3.5" />
                </span>
                <Button
                  variant="ghost"
                  size="icon-xs"
                  onClick={() => move(w.id, -1)}
                  disabled={idx === 0}
                  aria-label={intl.formatMessage({ id: 'home.layout.moveUp' })}
                >
                  <ArrowUp />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-xs"
                  onClick={() => move(w.id, 1)}
                  disabled={idx === visible.length - 1}
                  aria-label={intl.formatMessage({ id: 'home.layout.moveDown' })}
                >
                  <ArrowDown />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-xs"
                  className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                  onClick={() => setHidden(w.id, true)}
                  aria-label={intl.formatMessage({ id: 'home.layout.hide' })}
                >
                  <EyeOffIcon />
                </Button>
              </div>
            )}
            {renderWidget(w.id)}
          </div>
        ))}
      </div>

      {/* Edit mode: hidden / addable widgets drawer. */}
      {editing && (
        <Card>
          <CardHeader>
            <CardTitle>{intl.formatMessage({ id: 'home.layout.hiddenTitle' })}</CardTitle>
          </CardHeader>
          <div className="px-4">
            {hiddenWidgets.length === 0 && addableCustom.length === 0 ? (
              <p className="py-1 text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'home.layout.hiddenEmpty' })}
              </p>
            ) : (
              <div className="flex flex-wrap gap-2">
                {hiddenWidgets.map((w) => (
                  <Button key={w.id} variant="brandSubtle" size="sm" onClick={() => setHidden(w.id, false)}>
                    <Plus />
                    {widgetLabel(w.id)}
                  </Button>
                ))}
                {/* Custom widgets not yet on the board (mine + team-shared). */}
                {addableCustom.map((w) => (
                  <Button
                    key={`custom:${w.id}`}
                    variant="outline"
                    size="sm"
                    onClick={() => addCustom(w.id)}
                    title={w.description}
                  >
                    <Plus />
                    {w.title}
                  </Button>
                ))}
              </div>
            )}
            {catalog.length === 0 && (
              <p className="mt-2 text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'home.layout.catalogUnavailable' })}
              </p>
            )}
          </div>
        </Card>
      )}
    </div>
  );
}
