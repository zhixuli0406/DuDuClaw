import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams } from 'react-router';
import {
  ChevronRight,
  MessageSquare,
  Plus,
  Pause,
  Play,
  LogOut,
  RotateCcw,
  Archive,
  Settings,
  Shirt,
  Upload,
  Trash2,
  MoreHorizontal,
  Cpu,
  Server,
  Activity,
  ListTodo,
  CheckCircle2,
  Loader2,
  Ban,
} from 'lucide-react';
import {
  api,
  type AgentDetail,
  type TaskInfo,
  type ActivityEvent,
} from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAgentAvatarStore } from '@/stores/agent-avatar-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentGlyphState } from '@/stores/agent-activity-store';
import { readFileAsBase64 } from '@/lib/attachments';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  PageHeader,
  Button,
  Badge,
  Empty,
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  ActorAvatar,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  Skeleton,
  type ActorStatus,
} from '@/components/mds';
import { StatusIcon } from '@/components/ui';
import { agentTaskStats, isLiveState, type AgentTaskStats } from '@/components/agent';
import { OffboardDialog } from '@/components/agent/OffboardDialog';
import { WardrobeDialog } from '@/components/agent/WardrobeDialog';
import { computeMood } from '@/lib/mascot-mood';
import { toStatusKey } from '@/lib/task-status';
import { timeAgo, formatCents } from '@/lib/format';

/** 512 KB — matches the backend avatar cap (`agents.set_avatar`). */
const MAX_AVATAR_BYTES = 512 * 1024;
const AVATAR_ACCEPT = '.png,.jpg,.jpeg,.webp';
const AVATAR_MIME = new Set(['image/png', 'image/jpeg', 'image/webp']);

const TAB_IDS = ['overview', 'work', 'records'] as const;
type TabId = (typeof TAB_IDS)[number];

const MOOD_EMOJI: Record<string, string> = {
  focused: '🧐',
  relaxed: '😌',
  resting: '💤',
  offline: '😴',
};

function actorStatus(status: string, archived: boolean, live: boolean): ActorStatus {
  if (archived) return 'offline';
  if (status === 'active') return live ? 'busy' : 'online';
  if (status === 'paused') return 'busy';
  if (status === 'terminated') return 'error';
  return 'offline';
}

/**
 * AgentDetailPage (`/agents/:id/:tab`) — the Multica "員工" hero detail
 * (spec §5.3 式2). A border-bottom hero header (breadcrumb → avatar + name +
 * presence + meta) over a line-underline tab strip: 總覽 / 工作 / 紀錄. The
 * bust立繪 is gone — a plain ActorAvatar carries the identity, mood is a small
 * element inside the overview card, and per-tab content is mds-Card slim lists.
 */
export function AgentDetailPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const params = useParams();
  const id = params.id ?? '';
  const rawTab = params.tab ?? 'overview';
  const tab: TabId = (TAB_IDS as readonly string[]).includes(rawTab) ? (rawTab as TabId) : 'overview';

  const connectionState = useConnectionStore((s) => s.state);
  const { pauseAgent, resumeAgent, archiveAgent, unarchiveAgent, agents, fetchAgents } = useAgentsStore();
  const setAvatarCache = useAgentAvatarStore((s) => s.set);

  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [tasks, setTasks] = useState<TaskInfo[] | null>(null);
  const [activities, setActivities] = useState<ActivityEvent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [showOffboard, setShowOffboard] = useState(false);
  const [showWardrobe, setShowWardrobe] = useState(false);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const avatarFileRef = useRef<HTMLInputElement>(null);

  const glyph = useAgentGlyphState(id, detail?.status);
  const live = isLiveState(glyph);

  const loadDetail = useCallback(async () => {
    setLoading(true);
    try {
      const d = await api.agents.inspect(id);
      setDetail(d);
      setAvatarCache(id, d.avatar ?? null);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [id, intl, setAvatarCache]);

  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    loadDetail();
  }, [connectionState, id, loadDetail]);

  // Tasks power the win tally + the 工作 tab.
  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    api.tasks
      .list({ agent_id: id })
      .then((r) => setTasks(r?.tasks ?? []))
      .catch((e) => {
        console.warn('[api]', e);
        setTasks([]);
      });
  }, [connectionState, id]);

  // Activity ledger (總覽 live tail + 紀錄 tab). Loaded lazily on first need.
  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    if ((tab === 'overview' || tab === 'records') && activities === null) {
      api.activity
        .list({ agent_id: id, limit: 50 })
        .then((r) => setActivities(r?.events ?? []))
        .catch((e) => {
          console.warn('[api]', e);
          toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
          setActivities([]);
        });
    }
  }, [tab, connectionState, id, activities, intl]);

  // Roster (for the handoff target picker).
  useEffect(() => {
    if (connectionState === 'authenticated') fetchAgents();
  }, [connectionState, fetchAgents]);

  const setTab = (next: string) => navigate(`/agents/${encodeURIComponent(id)}/${next}`);

  const lifecycleAction = useCallback(
    async (fn: () => Promise<void>, successId: string) => {
      setBusy(true);
      try {
        await fn();
        toast.success(intl.formatMessage({ id: successId }, { name: detail?.display_name ?? id }));
        await loadDetail();
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(false);
      }
    },
    [detail, id, intl, loadDetail],
  );

  const handleAvatarFile = useCallback(
    async (file: File) => {
      if (!AVATAR_MIME.has(file.type)) {
        toast.error(intl.formatMessage({ id: 'agents.avatar.badType' }));
        return;
      }
      if (file.size > MAX_AVATAR_BYTES) {
        toast.error(intl.formatMessage({ id: 'agents.avatar.tooLarge' }));
        return;
      }
      setAvatarBusy(true);
      try {
        const b64 = await readFileAsBase64(file);
        const dataUri = `data:${file.type};base64,${b64}`;
        await api.agents.setAvatar(id, dataUri);
        setAvatarCache(id, dataUri);
        setDetail((d) => (d ? { ...d, avatar: dataUri, has_avatar: true } : d));
        toast.success(intl.formatMessage({ id: 'agents.avatar.saved' }));
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
      } finally {
        setAvatarBusy(false);
      }
    },
    [id, intl, setAvatarCache],
  );

  const handleAvatarClear = useCallback(async () => {
    setAvatarBusy(true);
    try {
      await api.agents.clearAvatar(id);
      setAvatarCache(id, null);
      setDetail((d) => (d ? { ...d, avatar: null, has_avatar: false } : d));
      toast.success(intl.formatMessage({ id: 'agents.avatar.removed' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setAvatarBusy(false);
    }
  }, [id, intl, setAvatarCache]);

  const stats = useMemo(() => agentTaskStats(tasks ?? [], id), [tasks, id]);

  if (loading && !detail) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <PageHeader hideTrigger>
          <Skeleton className="h-4 w-40" />
        </PageHeader>
        <div className="mx-auto w-full max-w-[1440px] space-y-4 p-4 sm:p-6">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-64 w-full" />
        </div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <PageHeader hideTrigger>
          <button
            onClick={() => navigate('/agents')}
            className="text-sm text-muted-foreground hover:text-foreground"
          >
            {intl.formatMessage({ id: 'nav.agents' })}
          </button>
        </PageHeader>
        <Empty
          icon={ListTodo}
          title={intl.formatMessage({ id: 'agentDetail.notFound' })}
          action={
            <Button variant="outline" size="sm" onClick={() => navigate('/agents')}>
              {intl.formatMessage({ id: 'agentDetail.back' })}
            </Button>
          }
        />
      </div>
    );
  }

  const archived = !!detail.archived;
  const isMain = detail.role === 'main';
  const roleTitle = detail.role
    ? intl.formatMessage({ id: `agents.role.${detail.role}` })
    : detail.name;

  // Mood — an honest one-member-company projection (display only).
  const moodKey =
    detail.status === 'paused'
      ? 'resting'
      : detail.status === 'terminated'
        ? 'offline'
        : computeMood({ total: 1, active: live ? 1 : 0, error: 0, inbox: 0 });

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      {/* Hero header (spec §5.3 式2). */}
      <div className="border-b border-surface-border px-4 pt-3 pb-5 sm:px-6">
        <div className="mx-auto w-full max-w-[1440px]">
          {/* Breadcrumb */}
          <nav className="mb-3 flex items-center gap-1 text-xs text-muted-foreground">
            <button onClick={() => navigate('/agents')} className="hover:text-foreground">
              {intl.formatMessage({ id: 'nav.agents' })}
            </button>
            <ChevronRight className="size-3" />
            <span className="truncate text-foreground">{detail.display_name}</span>
          </nav>

          <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
            <div className="flex min-w-0 items-start gap-4">
              <ActorAvatar
                actorType="agent"
                size="2xl"
                name={detail.display_name}
                src={detail.avatar ?? undefined}
                showStatusDot
                status={actorStatus(detail.status, archived, live)}
                className="ring-1"
              />
              <div className="min-w-0 space-y-1.5">
                <div className="flex flex-wrap items-center gap-2">
                  <h1 className="truncate text-xl font-semibold tracking-tight text-foreground sm:text-2xl">
                    {detail.display_name}
                  </h1>
                  <Badge
                    variant={
                      archived ? 'secondary' : detail.status === 'active' ? 'default' : 'secondary'
                    }
                  >
                    {intl.formatMessage({ id: `status.${archived ? 'archived' : detail.status}` })}
                  </Badge>
                </div>
                <p className="max-w-2xl truncate text-sm text-muted-foreground">
                  {roleTitle}
                  {detail.department ? ` · ${detail.department}` : ''}
                </p>
                {/* Meta row */}
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1 pt-0.5 text-xs text-muted-foreground">
                  {detail.model?.preferred && (
                    <span className="inline-flex items-center gap-1">
                      <Cpu className="size-3" />
                      <span className="font-mono">{detail.model.preferred}</span>
                    </span>
                  )}
                  {detail.model?.api_mode && (
                    <span className="inline-flex items-center gap-1">
                      <Server className="size-3" />
                      {intl.formatMessage({ id: `agents.apiMode.${detail.model.api_mode}` })}
                    </span>
                  )}
                </div>
              </div>
            </div>

            {/* Actions */}
            <div className="flex shrink-0 items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => navigate(`/chat?agent=${encodeURIComponent(id)}`)}
              >
                <MessageSquare />
                {intl.formatMessage({ id: 'agentDetail.action.chat' })}
              </Button>
              <Button
                variant="brand"
                size="sm"
                onClick={() => navigate(`/tasks?new=1&assignee=${encodeURIComponent(id)}`)}
              >
                <Plus />
                {intl.formatMessage({ id: 'agentDetail.action.delegate' })}
              </Button>
              <input
                ref={avatarFileRef}
                type="file"
                accept={AVATAR_ACCEPT}
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void handleAvatarFile(f);
                  e.target.value = '';
                }}
              />
              <DropdownMenu>
                <DropdownMenuTrigger
                  render={
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={intl.formatMessage({ id: 'agentDetail.more' })}
                    />
                  }
                >
                  <MoreHorizontal />
                </DropdownMenuTrigger>
                <DropdownMenuContent>
                  {!archived &&
                    (detail.status === 'active' ? (
                      <DropdownMenuItem
                        disabled={busy}
                        onClick={() => lifecycleAction(() => pauseAgent(id), 'agentDetail.rested')}
                      >
                        <Pause />
                        {intl.formatMessage({ id: 'agentDetail.rest' })}
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem
                        disabled={busy}
                        onClick={() => lifecycleAction(() => resumeAgent(id), 'agentDetail.resumed')}
                      >
                        <Play />
                        {intl.formatMessage({ id: 'agentDetail.resume' })}
                      </DropdownMenuItem>
                    ))}
                  <DropdownMenuItem onClick={() => setShowWardrobe(true)}>
                    <Shirt />
                    {intl.formatMessage({ id: 'wardrobe.open' })}
                  </DropdownMenuItem>
                  <DropdownMenuItem disabled={avatarBusy} onClick={() => avatarFileRef.current?.click()}>
                    <Upload />
                    {intl.formatMessage({ id: 'agents.avatar.upload' })}
                  </DropdownMenuItem>
                  {detail.has_avatar && (
                    <DropdownMenuItem disabled={avatarBusy} onClick={handleAvatarClear}>
                      <Trash2 />
                      {intl.formatMessage({ id: 'agents.avatar.remove' })}
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onClick={() => navigate(`/agents/${encodeURIComponent(id)}/edit`)}>
                    <Settings />
                    {intl.formatMessage({ id: 'agentDetail.editFull' })}
                  </DropdownMenuItem>
                  {archived ? (
                    <DropdownMenuItem
                      disabled={busy}
                      onClick={() => lifecycleAction(() => unarchiveAgent(id), 'agents.unarchive.done')}
                    >
                      <RotateCcw />
                      {intl.formatMessage({ id: 'agents.unarchive' })}
                    </DropdownMenuItem>
                  ) : (
                    !isMain && (
                      <>
                        <DropdownMenuItem
                          disabled={busy}
                          onClick={() => lifecycleAction(() => archiveAgent(id), 'agents.archive.done')}
                        >
                          <Archive />
                          {intl.formatMessage({ id: 'agents.archive' })}
                        </DropdownMenuItem>
                        <DropdownMenuItem variant="destructive" onClick={() => setShowOffboard(true)}>
                          <LogOut />
                          {intl.formatMessage({ id: 'agentDetail.dismiss' })}
                        </DropdownMenuItem>
                      </>
                    )
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
        </div>
      </div>

      {/* Archived banner */}
      {archived && (
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-surface-border bg-muted/50 px-4 py-2 sm:px-6">
          <span className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'agents.archived.notice' }, { name: detail.display_name })}
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => lifecycleAction(() => unarchiveAgent(id), 'agents.unarchive.done')}
          >
            <RotateCcw />
            {intl.formatMessage({ id: 'agents.unarchive' })}
          </Button>
        </div>
      )}

      {/* Tab strip (line underline). */}
      <Tabs value={tab} onValueChange={(v) => setTab(String(v))} variant="line" className="gap-0">
        <div className="border-b border-surface-border px-4 sm:px-6">
          <div className="mx-auto w-full max-w-[1440px]">
            <TabsList className="h-11">
              <TabsTab value="overview">{intl.formatMessage({ id: 'agentDetail.tab.overview' })}</TabsTab>
              <TabsTab value="work">{intl.formatMessage({ id: 'agentDetail.tab.work' })}</TabsTab>
              <TabsTab value="records">{intl.formatMessage({ id: 'agentDetail.tab.records' })}</TabsTab>
            </TabsList>
          </div>
        </div>

        <TabsPanel value="overview">
          <OverviewTab
            detail={detail}
            stats={stats}
            activities={activities}
            live={live}
            moodKey={moodKey}
          />
        </TabsPanel>
        <TabsPanel value="work">
          <WorkTab tasks={tasks} onOpen={(taskId) => navigate(`/tasks/${taskId}`)} />
        </TabsPanel>
        <TabsPanel value="records">
          <RecordsTab activities={activities} />
        </TabsPanel>
      </Tabs>

      <OffboardDialog
        open={showOffboard}
        agent={detail}
        candidates={agents.filter((a) => a.name !== id && !a.archived)}
        busy={busy}
        onClose={() => setShowOffboard(false)}
        onDone={() => {
          setShowOffboard(false);
          navigate('/agents');
        }}
      />

      {showWardrobe && (
        <WardrobeDialog
          agentId={detail.name}
          displayName={detail.display_name}
          outfit={detail.outfit ?? null}
          onClose={() => setShowWardrobe(false)}
          onSaved={() => {
            void loadDetail();
            void fetchAgents();
          }}
        />
      )}
    </div>
  );
}

// ── Overview ────────────────────────────────────────────────
function OverviewTab({
  detail,
  stats,
  activities,
  live,
  moodKey,
}: {
  detail: AgentDetail;
  stats: AgentTaskStats;
  activities: ReadonlyArray<ActivityEvent> | null;
  live: boolean;
  moodKey: string;
}) {
  const intl = useIntl();
  return (
    <div className="mx-auto w-full max-w-[1440px] p-4 sm:p-6">
      <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_320px]">
        {/* Left: mood + tally + live tail. */}
        <div className="space-y-6">
          {/* Mood (small element, no bust). */}
          <Card data-size="sm">
            <CardContent className="flex items-center gap-3">
              <span className="text-2xl" aria-hidden="true">
                {MOOD_EMOJI[moodKey] ?? '😌'}
              </span>
              <p className="text-sm text-foreground">
                {intl.formatMessage({ id: `agentDetail.mood.${moodKey}` })}
              </p>
            </CardContent>
          </Card>

          {/* Win tally. */}
          <div className="grid grid-cols-3 gap-3">
            <StatTile icon={CheckCircle2} tone="text-success" value={stats.done} label={intl.formatMessage({ id: 'agentDetail.stats.done' })} />
            <StatTile icon={Loader2} tone="text-info" value={stats.inProgress} label={intl.formatMessage({ id: 'agentDetail.stats.inProgress' })} />
            <StatTile icon={Ban} tone="text-destructive" value={stats.blocked} label={intl.formatMessage({ id: 'agentDetail.stats.blocked' })} />
          </div>

          {/* Live activity tail. */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-sm">
                {intl.formatMessage({ id: 'agentDetail.live.title' })}
                {live && <span className="size-1.5 rounded-full bg-success" />}
              </CardTitle>
            </CardHeader>
            <CardContent>
              {activities === null ? (
                <ActivitySkeleton />
              ) : activities.length === 0 ? (
                <Empty icon={Activity} variant="dashed" title={intl.formatMessage({ id: 'agentDetail.live.empty' })} />
              ) : (
                <ul className="space-y-1.5">
                  {activities.slice(0, 6).map((ev) => (
                    <li key={ev.id} className="flex items-baseline gap-2 text-sm">
                      <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                        {timeAgo(ev.timestamp)}
                      </span>
                      <span className="min-w-0 flex-1 truncate text-foreground">{ev.summary}</span>
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>
        </div>

        {/* Right: summary property rows. */}
        <Card className="h-fit">
          <CardHeader>
            <CardTitle className="text-sm">{intl.formatMessage({ id: 'agentDetail.summary.title' })}</CardTitle>
          </CardHeader>
          <CardContent className="divide-y divide-surface-border">
            <PropertyRow label={intl.formatMessage({ id: 'agents.inspect.status' })}>
              {intl.formatMessage({ id: `status.${detail.archived ? 'archived' : detail.status}` })}
            </PropertyRow>
            <PropertyRow label={intl.formatMessage({ id: 'agentDetail.field.model' })}>
              <span className="font-mono text-xs">{detail.model?.preferred || '—'}</span>
            </PropertyRow>
            <PropertyRow label={intl.formatMessage({ id: 'agentDetail.summary.runtime' })}>
              {detail.model?.api_mode
                ? intl.formatMessage({ id: `agents.apiMode.${detail.model.api_mode}` })
                : '—'}
            </PropertyRow>
            <PropertyRow label={intl.formatMessage({ id: 'agentDetail.field.skills' })}>
              <span className="font-mono tabular-nums">{detail.skills?.length ?? 0}</span>
            </PropertyRow>
            {detail.department && (
              <PropertyRow label={intl.formatMessage({ id: 'agents.department.label' })}>
                {detail.department}
              </PropertyRow>
            )}
            <PropertyRow label={intl.formatMessage({ id: 'agentDetail.overview.budget' })}>
              <span className="font-mono text-xs tabular-nums">
                {formatCents(detail.budget?.spent_cents)} / {formatCents(detail.budget?.monthly_limit_cents)}
              </span>
            </PropertyRow>
            <PropertyRow label={intl.formatMessage({ id: 'agentDetail.field.heartbeat' })}>
              {detail.heartbeat?.enabled
                ? intl.formatMessage({ id: 'common.enabled' })
                : intl.formatMessage({ id: 'common.disabled' })}
            </PropertyRow>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

// ── Work (task list) ────────────────────────────────────────
function WorkTab({
  tasks,
  onOpen,
}: {
  tasks: ReadonlyArray<TaskInfo> | null;
  onOpen: (taskId: string) => void;
}) {
  const intl = useIntl();
  const rows = useMemo(
    () =>
      [...(tasks ?? [])].sort(
        (a, b) => new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime(),
      ),
    [tasks],
  );

  return (
    <div className="mx-auto w-full max-w-[1440px] p-4 sm:p-6">
      {tasks === null ? (
        <ActivitySkeleton />
      ) : rows.length === 0 ? (
        <Empty icon={ListTodo} title={intl.formatMessage({ id: 'agentDetail.work.empty' })} />
      ) : (
        <ul className="overflow-hidden rounded-xl border border-surface-border bg-surface">
          {rows.map((t) => (
            <li
              key={t.id}
              onClick={() => onOpen(t.id)}
              className="flex h-9 cursor-pointer items-center gap-2.5 border-b border-surface-border px-4 text-sm transition-colors last:border-b-0 hover:bg-surface-hover"
            >
              <StatusIcon status={toStatusKey(t.status)} size="sm" />
              <span className="min-w-0 flex-1 truncate text-foreground">{t.title}</span>
              <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                {timeAgo(t.updated_at)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ── Records (activity ledger) ───────────────────────────────
function RecordsTab({ activities }: { activities: ReadonlyArray<ActivityEvent> | null }) {
  const intl = useIntl();
  return (
    <div className="mx-auto w-full max-w-[1440px] p-4 sm:p-6">
      {activities === null ? (
        <ActivitySkeleton />
      ) : activities.length === 0 ? (
        <Empty icon={Activity} title={intl.formatMessage({ id: 'agentDetail.records.empty' })} />
      ) : (
        <ul className="overflow-hidden rounded-xl border border-surface-border bg-surface">
          {activities.map((ev) => (
            <li
              key={ev.id}
              className="flex h-9 items-center gap-2.5 border-b border-surface-border px-4 text-sm last:border-b-0"
            >
              <span className="min-w-0 flex-1 truncate text-foreground">{ev.summary}</span>
              <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                {timeAgo(ev.timestamp)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ── Small pieces ────────────────────────────────────────────
function PropertyRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 py-2.5 first:pt-0 last:pb-0">
      <dt className="shrink-0 text-xs text-muted-foreground">{label}</dt>
      <dd className="min-w-0 truncate text-right text-sm text-foreground">{children}</dd>
    </div>
  );
}

function StatTile({
  icon: Icon,
  tone,
  value,
  label,
}: {
  icon: React.ComponentType<{ className?: string }>;
  tone: string;
  value: number;
  label: string;
}) {
  return (
    <Card data-size="sm">
      <CardContent className="flex flex-col items-center gap-1 text-center">
        <Icon className={cn('size-5', tone)} />
        <span className="text-2xl font-semibold tabular-nums text-foreground">{value}</span>
        <span className="text-xs text-muted-foreground">{label}</span>
      </CardContent>
    </Card>
  );
}

function ActivitySkeleton() {
  return (
    <div className="space-y-2">
      {Array.from({ length: 3 }).map((_, i) => (
        <Skeleton key={i} className="h-8 w-full" />
      ))}
    </div>
  );
}
