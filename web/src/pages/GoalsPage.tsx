import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import { Target, Plus, RefreshCw, Check, Trash2, Hand, Search, Pin } from 'lucide-react';

import { api, type TaskInfo } from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { useAssignStore } from '@/stores/assign-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { PauseReasonChip, TaskListActionsMenu, RenameTaskDialog } from '@/components/task';
import {
  ActorAvatar,
  Button,
  Card,
  CardContent,
  CollectionPageState,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Spinner,
  Switch,
  useErrorMessage,
} from '@/components/mds';

/**
 * GoalsPage (`/goals`) — assign and manage autonomous goal-loop tasks.
 *
 * Everything the channel `/goal` command can do, from the dashboard: assign
 * a goal to an AI employee (with acceptance criteria), watch every judge
 * round of the loop, and intervene at the human nodes — pending kickoff
 * approvals (collaborator/consultant autonomy) and `needs_human`
 * escalations (retry / mark done / abandon / take over). Interventions go
 * through the same fail-closed path as the channel buttons
 * (`tasks.goal_decide`), never a bare status write.
 *
 * I-2c (2026-08-15): the per-task detail used to live in a page-local dialog
 * (`GoalDetailDialog`, `?task=<id>`) with its own timeline/contract/kickoff
 * rendering — a second detail UI diverging from `/tasks/:id`'s four-tab
 * page. That content now lives in `/tasks/:id` (via the shared
 * `GoalLoopPanel` pieces), so opening a task here just navigates there;
 * `?task=<id>` is kept as a redirect-only compatibility shim for existing
 * entry points (`AssignSheet` still lands on this URL after creating a goal).
 */

const ACTIVE_STATUSES = ['todo', 'pending', 'in_progress', 'review', 'revising'] as const;
const WAITING_STATUSES = ['needs_human'] as const;
const FINISHED_STATUSES = ['done', 'cancelled', 'failed', 'blocked'] as const;

/** I-3b: page size for the 已結束／已封存 section's "load more" pagination
 *  (replaces the old client-side `.slice(0, 20)` hard cutoff). */
const LIST_PAGE_SIZE = 20;

function statusTone(status: string): string {
  switch (status) {
    case 'needs_human':
      return 'text-amber-600 dark:text-amber-400';
    case 'done':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'cancelled':
    case 'failed':
      return 'text-rose-600 dark:text-rose-400';
    case 'review':
    case 'revising':
      return 'text-chart-1';
    default:
      return 'text-muted-foreground';
  }
}

function GoalStatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  return (
    <span className={`text-xs font-medium ${statusTone(status)}`}>
      {intl.formatMessage({ id: `goals.status.${status}`, defaultMessage: status })}
    </span>
  );
}

/** needs_human intervention buttons, shared by list cards and the detail. */
function InterventionButtons({
  task,
  compact,
  onDecided,
}: {
  task: TaskInfo;
  compact?: boolean;
  onDecided: () => void;
}) {
  const intl = useIntl();
  const [busy, setBusy] = useState<string | null>(null);
  const [noteFor, setNoteFor] = useState<'retry' | null>(null);
  const [note, setNote] = useState('');

  const decide = async (action: 'retry' | 'done' | 'abort' | 'takeover', withNote?: string) => {
    setBusy(action);
    try {
      const r = await api.tasks.goalDecide(task.id, action, withNote);
      toast.success(r.message);
      setNoteFor(null);
      setNote('');
      onDecided();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1.5">
        <Button
          variant="outline"
          size="sm"
          disabled={busy !== null}
          onClick={() => setNoteFor(noteFor === 'retry' ? null : 'retry')}
        >
          <RefreshCw className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.retry' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('done')}>
          <Check className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.done' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('abort')}>
          <Trash2 className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.abort' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('takeover')}>
          <Hand className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.takeover' })}
        </Button>
      </div>
      {noteFor === 'retry' && (
        <div className="flex gap-1.5">
          <Input
            value={note}
            placeholder={intl.formatMessage({ id: 'goals.decide.notePlaceholder' })}
            onChange={(e) => setNote(e.target.value)}
            className={compact ? 'h-8 text-xs' : ''}
          />
          <Button variant="brand" size="sm" disabled={busy !== null} onClick={() => decide('retry', note)}>
            {intl.formatMessage({ id: 'goals.decide.retryGo' })}
          </Button>
        </div>
      )}
    </div>
  );
}

function GoalCard({
  task,
  onOpen,
  onChanged,
  onTogglePin,
  onToggleArchive,
  onRename,
}: {
  task: TaskInfo;
  onOpen: () => void;
  onChanged: () => void;
  onTogglePin: () => void;
  onToggleArchive: () => void;
  onRename: () => void;
}) {
  const intl = useIntl();
  return (
    <Card data-size="sm" className="transition-colors hover:border-surface-border-strong">
      <CardContent className="space-y-2 py-3">
        <div className="flex items-start gap-1">
          <button type="button" onClick={onOpen} className="block min-w-0 flex-1 text-left">
            <div className="flex items-center gap-2">
              <ActorAvatar actorType="agent" size="xs" name={task.assigned_to} />
              <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{task.title}</span>
              {task.pinned && <Pin className="size-3 shrink-0 text-amber-500" aria-hidden="true" />}
              <GoalStatusBadge status={task.status} />
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
              <span>
                {intl.formatMessage({ id: 'goals.roundBadge' }, { n: (task.revision_round ?? 0) + 1 })}
              </span>
              {task.diminishing && (
                <span className="text-amber-600 dark:text-amber-400">
                  {intl.formatMessage({ id: 'goals.diminishing' })}
                </span>
              )}
              {task.claimed_by?.startsWith('dashboard:') || task.claimed_by?.startsWith('channel:') ? (
                <span className="text-chart-1">{intl.formatMessage({ id: 'goals.takenOver' })}</span>
              ) : null}
              <span className="ml-auto font-mono tabular-nums">{timeAgo(task.updated_at)}</span>
            </div>
          </button>
          <TaskListActionsMenu
            task={task}
            onTogglePin={onTogglePin}
            onToggleArchive={onToggleArchive}
            onRename={onRename}
          />
        </div>
        {task.status === 'needs_human' && (
          <div className="border-t border-surface-border pt-2">
            <div className="mb-1.5">
              <PauseReasonChip reason={task.pause_reason} />
            </div>
            {task.judge_feedback && (
              <p className="mb-1.5 line-clamp-2 text-xs text-muted-foreground">{task.judge_feedback}</p>
            )}
            <InterventionButtons task={task} compact onDecided={onChanged} />
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** One row in the 已結束／已封存 list — same kebab as `GoalCard`, flat layout. */
function GoalListRow({
  task,
  onOpen,
  onTogglePin,
  onToggleArchive,
  onRename,
}: {
  task: TaskInfo;
  onOpen: () => void;
  onTogglePin: () => void;
  onToggleArchive: () => void;
  onRename: () => void;
}) {
  return (
    <div className="flex items-center gap-2 py-2 text-xs">
      <button type="button" onClick={onOpen} className="flex min-w-0 flex-1 items-center gap-2 text-left">
        {task.pinned && <Pin className="size-3 shrink-0 text-amber-500" aria-hidden="true" />}
        <span className="min-w-0 flex-1 truncate text-foreground">{task.title}</span>
        <GoalStatusBadge status={task.status} />
        <span className="font-mono tabular-nums text-muted-foreground">
          {timeAgo(task.completed_at ?? task.updated_at)}
        </span>
      </button>
      <TaskListActionsMenu
        task={task}
        onTogglePin={onTogglePin}
        onToggleArchive={onToggleArchive}
        onRename={onRename}
      />
    </div>
  );
}

export function GoalsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const errorText = useErrorMessage();
  // UX plan I-1a: 交辦 opens the one shared panel. The seven-field
  // `CreateGoalDialog` that used to live here was the only form that actually
  // drove `tasks.goal_create` and nothing in the app linked to it; the
  // AssignSheet carries every one of its fields (the advanced four behind
  // 更多設定) and is reachable from every entry point.
  const openAssign = useAssignStore((s) => s.openAssign);
  const connState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents, loaded: agentsLoaded } = useAgentsStore();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [searchParams] = useSearchParams();
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [agentFilter, setAgentFilter] = useState('');
  const [search, setSearch] = useState('');
  const [renameTarget, setRenameTarget] = useState<TaskInfo | null>(null);

  // I-3b: 已結束／已封存 — a separate paginated fetch (`tasks.list_page`)
  // replacing the old `.slice(0, 20)` client-side cutoff on `tasks`. Two
  // modes share one state slice: `showArchived=false` pages through the four
  // FINISHED_STATUSES (excluding archived rows, one `list_page` call per
  // status so an active/waiting-heavy account can't push finished rows off
  // the page); `showArchived=true` switches to a single unfiltered-by-status
  // `archived: true` query — "browse everything I've archived", regardless
  // of what status it was in when archived (a `needs_human` or `in_progress`
  // task can be archived too; the plain `tasks.list` used for the sections
  // above always excludes archived rows, so this toggle is the only way back
  // to one of those).
  const [showArchived, setShowArchived] = useState(false);
  const [listTasks, setListTasks] = useState<TaskInfo[]>([]);
  const [listHasMore, setListHasMore] = useState(false);
  const [listLoading, setListLoading] = useState(true);
  const [listLoadingMore, setListLoadingMore] = useState(false);
  const finishedOffsetsRef = useRef<Record<string, number>>({});
  const archivedOffsetRef = useRef(0);

  // Non-admins must scope tasks.list to a bound agent (check_agent_filter) —
  // an unscoped first load would error out. `agents.list` is already
  // binding-filtered server-side, so its first entry is a valid default scope.
  useEffect(() => {
    if (!isAdmin && !agentFilter && agents.length > 0) {
      setAgentFilter(agents[0].name);
    }
  }, [isAdmin, agentFilter, agents]);

  const load = useCallback(async () => {
    if (!isAdmin && !agentFilter) {
      // Waiting for the default scope above; a non-admin with zero bound
      // agents will never get one — settle into the empty state instead of
      // an endless skeleton.
      if (agentsLoaded && agents.length === 0) setLoading(false);
      return;
    }
    try {
      const r = await api.tasks.list({
        goal_mode: true,
        ...(agentFilter ? { agent_id: agentFilter } : {}),
      });
      setTasks((r.tasks ?? []).filter((t) => t.goal_mode));
      setLoadError(null);
    } catch (e) {
      console.warn('[api]', e);
      setLoadError(errorText(e));
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
    } finally {
      setLoading(false);
    }
    // errorText stable; intl from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentFilter, isAdmin, agentsLoaded, agents.length]);

  const loadList = useCallback(
    async (reset: boolean) => {
      if (!isAdmin && !agentFilter) {
        if (reset) {
          setListTasks([]);
          setListHasMore(false);
          setListLoading(false);
        }
        return;
      }
      (reset ? setListLoading : setListLoadingMore)(true);
      try {
        if (showArchived) {
          if (reset) archivedOffsetRef.current = 0;
          const r = await api.tasks.listPage({
            goal_mode: true,
            ...(agentFilter ? { agent_id: agentFilter } : {}),
            archived: true,
            limit: LIST_PAGE_SIZE,
            offset: archivedOffsetRef.current,
          });
          const rows = r.tasks ?? [];
          archivedOffsetRef.current += rows.length;
          setListHasMore(archivedOffsetRef.current < (r.total ?? 0));
          setListTasks((prev) => {
            const base = reset ? [] : prev;
            const seen = new Set(base.map((t) => t.id));
            return [...base, ...rows.filter((t) => !seen.has(t.id))];
          });
        } else {
          if (reset) finishedOffsetsRef.current = {};
          const offsets = finishedOffsetsRef.current;
          const results = await Promise.all(
            FINISHED_STATUSES.map((status) =>
              api.tasks.listPage({
                status,
                goal_mode: true,
                ...(agentFilter ? { agent_id: agentFilter } : {}),
                archived: false,
                limit: LIST_PAGE_SIZE,
                offset: offsets[status] ?? 0,
              }),
            ),
          );
          let hasMore = false;
          let fetched: TaskInfo[] = [];
          const nextOffsets: Record<string, number> = { ...offsets };
          results.forEach((r, i) => {
            const status = FINISHED_STATUSES[i];
            const rows = r.tasks ?? [];
            nextOffsets[status] = (offsets[status] ?? 0) + rows.length;
            if (nextOffsets[status] < (r.total ?? 0)) hasMore = true;
            fetched = fetched.concat(rows);
          });
          finishedOffsetsRef.current = nextOffsets;
          setListHasMore(hasMore);
          setListTasks((prev) => {
            const base = reset ? [] : prev;
            const seen = new Set(base.map((t) => t.id));
            const merged = [...base, ...fetched.filter((t) => !seen.has(t.id))];
            // Backend sorts each per-status page `pinned DESC, updated_at
            // DESC`; merging four pages needs one more pass to keep that
            // order globally.
            merged.sort((a, b) => {
              if (!!a.pinned !== !!b.pinned) return a.pinned ? -1 : 1;
              return (b.updated_at || '').localeCompare(a.updated_at || '');
            });
            return merged;
          });
        }
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
      } finally {
        (reset ? setListLoading : setListLoadingMore)(false);
      }
      // errorText/intl stable from context.
      // eslint-disable-next-line react-hooks/exhaustive-deps
    },
    [agentFilter, isAdmin, showArchived],
  );

  useEffect(() => {
    if (connState === 'authenticated') {
      fetchAgents();
      void load();
      void loadList(true);
    }
    // `load`/`loadList` already depend on everything relevant.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connState, fetchAgents, load, loadList]);

  // I-2c: `/goals?task=<id>` used to open a page-local detail dialog; the
  // dialog is gone and `/tasks/:id` is the single detail surface now. Kept
  // as a redirect so existing links (AssignSheet lands here right after
  // `tasks.goal_create`) keep working.
  const redirectTaskId = searchParams.get('task');
  useEffect(() => {
    if (redirectTaskId) navigate(`/tasks/${redirectTaskId}`, { replace: true });
  }, [redirectTaskId, navigate]);

  const openDetail = useCallback((id: string) => navigate(`/tasks/${id}`), [navigate]);

  const handleTogglePin = useCallback(
    async (task: TaskInfo) => {
      try {
        await (task.pinned ? api.tasks.unpin(task.id) : api.tasks.pin(task.id));
        toast.success(intl.formatMessage({ id: task.pinned ? 'goals.unpin.success' : 'goals.pin.success' }));
        void load();
        void loadList(true);
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [intl, load, loadList],
  );

  const handleToggleArchive = useCallback(
    async (task: TaskInfo) => {
      try {
        await (task.archived ? api.tasks.unarchive(task.id) : api.tasks.archive(task.id));
        toast.success(
          intl.formatMessage({ id: task.archived ? 'goals.unarchive.success' : 'goals.archive.success' }),
        );
        void load();
        void loadList(true);
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [intl, load, loadList],
  );

  const handleRenamed = useCallback(() => {
    void load();
    void loadList(true);
  }, [load, loadList]);

  const waiting = tasks.filter((t) => (WAITING_STATUSES as readonly string[]).includes(t.status));
  const active = tasks.filter((t) => (ACTIVE_STATUSES as readonly string[]).includes(t.status));

  // I-3b: free-text title search across every section. Scoped to what's
  // currently loaded — `waiting`/`active` are always fully loaded (no
  // pagination there), `listTasks` covers whatever "load more" has pulled in
  // so far for the 已結束／已封存 section.
  const searchNorm = search.trim().toLowerCase();
  const matchesSearch = useCallback(
    (t: TaskInfo) => !searchNorm || t.title.toLowerCase().includes(searchNorm),
    [searchNorm],
  );
  const filteredWaiting = waiting.filter(matchesSearch);
  const filteredActive = active.filter(matchesSearch);
  const filteredList = listTasks.filter(matchesSearch);
  const noSearchMatches =
    searchNorm.length > 0 &&
    filteredWaiting.length === 0 &&
    filteredActive.length === 0 &&
    filteredList.length === 0 &&
    (tasks.length > 0 || listTasks.length > 0);

  const agentLabel = agentFilter
    ? agents.find((a) => a.name === agentFilter)?.display_name || agentFilter
    : intl.formatMessage({ id: 'foresight.allAgents' });

  // Only declare "nothing here at all" once BOTH fetches have had a chance to
  // answer — otherwise an account with zero active/waiting but non-zero
  // finished/archived history would flash the big empty state before the
  // second fetch resolves.
  const bigEmpty = tasks.length === 0 && (listLoading ? false : listTasks.length === 0);
  const waitingForListToRuleOutEmpty = tasks.length === 0 && listLoading;

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Target className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'goals.title' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'goals.subtitle' })}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Select value={agentFilter} onValueChange={(v) => setAgentFilter(String(v))}>
            <SelectTrigger className="w-44">
              <SelectValue>{agentLabel}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {/* Non-admins cannot query unscoped (check_agent_filter) — hide "all". */}
              {isAdmin && (
                <SelectItem value="">{intl.formatMessage({ id: 'foresight.allAgents' })}</SelectItem>
              )}
              {agents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button variant="brand" size="sm" onClick={() => openAssign()}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'goals.assign' })}</span>
          </Button>
        </div>
      </div>

      {loading || waitingForListToRuleOutEmpty ? (
        <CollectionPageState state="loading" />
      ) : loadError ? (
        <CollectionPageState
          state="error"
          icon={Target}
          title={intl.formatMessage({ id: 'goals.error.title' })}
          description={loadError}
          action={
            <Button variant="outline" size="sm" onClick={() => void load()}>
              {intl.formatMessage({ id: 'foresight.error.retry' })}
            </Button>
          }
        />
      ) : bigEmpty ? (
        <CollectionPageState
          state="empty"
          icon={Target}
          title={intl.formatMessage({ id: 'goals.empty.title' })}
          description={intl.formatMessage({ id: 'goals.empty.desc' })}
        />
      ) : (
        <>
          {/* I-3b: search across every section + the 已封存 visibility toggle. */}
          <div className="flex flex-wrap items-center gap-3 border-b border-surface-border pb-3">
            <div className="relative min-w-0 flex-1 sm:max-w-xs">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={intl.formatMessage({ id: 'goals.search.placeholder' })}
                aria-label={intl.formatMessage({ id: 'goals.search.placeholder' })}
                className="pl-8"
                autoComplete="off"
                spellCheck={false}
              />
            </div>
            <label className="flex items-center gap-2 text-sm text-muted-foreground">
              <Switch checked={showArchived} onCheckedChange={(v) => setShowArchived(Boolean(v))} />
              {intl.formatMessage({ id: 'goals.filter.showArchived' })}
            </label>
          </div>

          {noSearchMatches ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'goals.search.empty' })}
            </p>
          ) : (
            <>
              {filteredWaiting.length > 0 && (
                <section className="space-y-2">
                  <h2 className="text-sm font-medium text-amber-600 dark:text-amber-400">
                    {intl.formatMessage({ id: 'goals.section.waiting' }, { n: filteredWaiting.length })}
                  </h2>
                  <div className="grid gap-3 lg:grid-cols-2">
                    {filteredWaiting.map((t) => (
                      <GoalCard
                        key={t.id}
                        task={t}
                        onOpen={() => openDetail(t.id)}
                        onChanged={load}
                        onTogglePin={() => void handleTogglePin(t)}
                        onToggleArchive={() => void handleToggleArchive(t)}
                        onRename={() => setRenameTarget(t)}
                      />
                    ))}
                  </div>
                </section>
              )}

              <section className="space-y-2">
                <h2 className="text-sm font-medium text-foreground">
                  {intl.formatMessage({ id: 'goals.section.active' }, { n: filteredActive.length })}
                </h2>
                {filteredActive.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'goals.section.activeEmpty' })}
                  </p>
                ) : (
                  <div className="grid gap-3 lg:grid-cols-2">
                    {filteredActive.map((t) => (
                      <GoalCard
                        key={t.id}
                        task={t}
                        onOpen={() => openDetail(t.id)}
                        onChanged={load}
                        onTogglePin={() => void handleTogglePin(t)}
                        onToggleArchive={() => void handleToggleArchive(t)}
                        onRename={() => setRenameTarget(t)}
                      />
                    ))}
                  </div>
                )}
              </section>

              <section className="space-y-2">
                <h2 className="text-sm font-medium text-muted-foreground">
                  {intl.formatMessage({ id: showArchived ? 'goals.section.archived' : 'goals.section.finished' })}
                </h2>
                {listLoading ? (
                  <div className="flex justify-center py-6">
                    <Spinner />
                  </div>
                ) : filteredList.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({
                      id: showArchived ? 'goals.section.archivedEmpty' : 'goals.section.finishedEmpty',
                    })}
                  </p>
                ) : (
                  <Card data-size="sm">
                    <CardContent className="divide-y divide-surface-border">
                      {filteredList.map((t) => (
                        <GoalListRow
                          key={t.id}
                          task={t}
                          onOpen={() => openDetail(t.id)}
                          onTogglePin={() => void handleTogglePin(t)}
                          onToggleArchive={() => void handleToggleArchive(t)}
                          onRename={() => setRenameTarget(t)}
                        />
                      ))}
                    </CardContent>
                  </Card>
                )}
                {listHasMore && !searchNorm && (
                  <div className="flex justify-center pt-1">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={listLoadingMore}
                      onClick={() => void loadList(false)}
                    >
                      {listLoadingMore && <Spinner className="size-3.5" />}
                      {intl.formatMessage({ id: 'activity.loadMore' })}
                    </Button>
                  </div>
                )}
              </section>
            </>
          )}
        </>
      )}

      <RenameTaskDialog task={renameTarget} onClose={() => setRenameTarget(null)} onRenamed={handleRenamed} />
    </div>
  );
}
