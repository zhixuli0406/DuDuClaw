import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams } from 'react-router';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import {
  BreadcrumbHeader,
  Button,
  Badge,
  Empty,
  ErrorState,
  ActorAvatar,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  Textarea,
  type BreadcrumbSegment,
} from '@/components/mds';
import { StatusIcon, InlineEditor, LiveBadge, usePanel } from '@/components/ui';
import {
  CreateTaskModal,
  TaskProperties,
  TaskBottomTabs,
  TaskDoneBurst,
  celebrateTaskDone,
  PauseReasonChip,
  GoalTakeoverButton,
  GoalContractCards,
  GoalPendingKickoff,
  GoalRoundTimeline,
  GoalRoundBadge,
  GoalDiminishingBadge,
  GoalDeadlineBadge,
} from '@/components/task';
import { OpenInChannelButton } from '@/components/inbox/OpenInChannelButton';
import { NeedsHumanActions } from '@/components/inbox/NeedsHumanTaskPanel';
import { toStatusKey } from '@/lib/task-status';
import { timeAgo, formatHms, secondsBetween } from '@/lib/format';
import { api, type TaskInfo, type TaskStatus, type TaskPriority, type GoalTimeline } from '@/lib/api';
import {
  ArrowLeft,
  Link2,
  PanelRight,
  MoreHorizontal,
  Trash2,
  Plus,
  ClipboardList,
  ChevronRight,
  CircleCheck,
  MessageSquareWarning,
  Loader2,
  RotateCcw,
  Pin,
  PinOff,
  Archive,
  ArchiveRestore,
} from 'lucide-react';
import { toast, formatError } from '@/lib/toast';

type TaskSource = 'channel' | 'delegated' | 'manual';
function taskSource(task: TaskInfo): TaskSource {
  if (task.message_id) return 'channel';
  if (task.parent_task_id) return 'delegated';
  return 'manual';
}

/** `/tasks/:id` — the Multica IssueDetail flagship (spec §5.3 式1). */
export function TaskDetailPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  // Same store, same P05 Blocker as the board: nothing here read `error`, so a
  // failed edit / assign / comment / delete was invisible — and the delete even
  // navigated away as if it had worked.
  const {
    tasks,
    activities,
    comments,
    loading,
    error,
    clearError,
    fetchTasks,
    updateTask,
    removeTask,
    assignTask,
    createTask,
    fetchActivities,
    fetchComments,
    addComment,
  } = useTasksStore();
  const { agents, fetchAgents } = useAgentsStore();
  const currentUser = useAuthStore((s) => s.user);
  const { setPanel, clearPanel, setSheetOpen, toggleCollapsed } = usePanel();

  const [addSub, setAddSub] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [burst, setBurst] = useState<{ agentId: string } | null>(null);
  // I-2c: the whole goal-loop story (rounds + activity + pending kickoff +
  // run linkage) — replaces the plain `tasks.iterations` fetch this page used
  // before the `/goals?task=` dialog's content was merged in. Best-effort —
  // a task with no goal rounds simply returns an empty timeline and the
  // goal-specific sections below render nothing.
  const [timeline, setTimeline] = useState<GoalTimeline | null>(null);

  useEffect(() => {
    fetchTasks();
    fetchAgents();
    fetchActivities({ limit: 100 });
  }, [fetchTasks, fetchAgents, fetchActivities]);

  useEffect(() => {
    if (id) fetchComments(id);
  }, [id, fetchComments]);

  const loadTimeline = useCallback(() => {
    if (!id) return;
    api.tasks
      .timeline(id)
      .then(setTimeline)
      .catch(() => setTimeline(null));
  }, [id]);

  useEffect(() => {
    loadTimeline();
  }, [loadTimeline]);

  const iterations = timeline?.iterations ?? [];

  const task = useMemo(() => tasks.find((t) => t.id === id), [tasks, id]);
  const parent = useMemo(
    () => (task?.parent_task_id ? tasks.find((t) => t.id === task.parent_task_id) : undefined),
    [tasks, task?.parent_task_id],
  );
  const subtasks = useMemo(() => (id ? tasks.filter((t) => t.parent_task_id === id) : []), [tasks, id]);
  const taskActivities = useMemo(() => activities.filter((e) => e.task_id === id), [activities, id]);
  const taskComments = useMemo(() => (id ? comments[id] ?? [] : []), [comments, id]);

  const assigneeAgent = agents.find((a) => a.name === task?.assigned_to);

  // ── Writers ──────────────────────────────────────────────
  const applyStatus = useCallback(
    (next: TaskStatus) => {
      if (!task || next === task.status) return;
      if (next === 'done') {
        celebrateTaskDone(intl.formatMessage({ id: 'tasks.celebrate.done' }));
        if (task.assigned_to) setBurst({ agentId: task.assigned_to });
      }
      updateTask(task.id, { status: next });
    },
    [task, updateTask, intl],
  );
  const applyPriority = useCallback(
    (next: TaskPriority) => {
      if (task) updateTask(task.id, { priority: next });
    },
    [task, updateTask],
  );
  const applyAssign = useCallback(
    (agentName: string) => {
      if (task) assignTask(task.id, agentName);
    },
    [task, assignTask],
  );

  // WP-A (§2-6): a task parked on a human decision is resolved ONLY through the
  // three choices below (重試 / 標記完成 / 放棄) — the same ones the inbox and the
  // channel buttons offer. The generic status picker never handled it correctly
  // anyway: `pending` (= 重試) was not among its options at all.
  const needsHuman = task?.status === 'needs_human';

  // I-3a "接著做": a goal-mode task that already reached a terminal state
  // (done / failed / cancelled) can take a follow-up message and be reopened
  // for another round — WorkBuddy's "already-finished tasks can take a
  // follow-up message" pattern (design doc §3.3). Unlike WorkBuddy the
  // reopened round still goes through the MAV judge (see `GoalContinuePanel`'s
  // hint copy) — this is intentional, not a limitation.
  const canGoalContinue =
    !!task?.goal_mode && (task?.status === 'done' || task?.status === 'failed' || task?.status === 'cancelled');

  // ── Right-hand PropertiesPanel (shell column, spec §5.3 式1 right 320) ──
  useEffect(() => () => clearPanel(), [clearPanel]);
  useEffect(() => {
    if (!task) return;
    setPanel({
      title: intl.formatMessage({ id: 'tasks.props.title' }),
      content: (
        <TaskProperties
          task={task}
          agents={agents}
          statusLocked={needsHuman}
          onStatusChange={applyStatus}
          onPriorityChange={applyPriority}
          onAssign={applyAssign}
        />
      ),
    });
  }, [task, agents, needsHuman, setPanel, intl, applyStatus, applyPriority, applyAssign]);

  const copyLink = useCallback(() => {
    try {
      void navigator.clipboard?.writeText(window.location.href);
      toast.success(intl.formatMessage({ id: 'tasks.detail.linkCopied' }));
    } catch {
      /* clipboard blocked — silent */
    }
  }, [intl]);

  const handleRemove = useCallback(async () => {
    if (!task) return;
    clearError();
    await removeTask(task.id);
    // Navigating away on a failed delete is the worst outcome available: the
    // task is still there and the user is told nothing.
    if (useTasksStore.getState().error != null) {
      toast.error(intl.formatMessage({ id: 'tasks.remove.failed' }));
      return;
    }
    setConfirmRemove(false);
    navigate('/tasks');
  }, [task, removeTask, navigate, clearError, intl]);

  // I-3b: pin/archive from the detail page too — the same actions the
  // `/goals` list kebab offers, so a task reached directly (a bookmark, a
  // channel deep-link) is never a dead end for un-archiving it.
  const togglePin = useCallback(async () => {
    if (!task) return;
    try {
      await (task.pinned ? api.tasks.unpin(task.id) : api.tasks.pin(task.id));
      await fetchTasks();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [task, fetchTasks, intl]);

  const toggleArchive = useCallback(async () => {
    if (!task) return;
    try {
      await (task.archived ? api.tasks.unarchive(task.id) : api.tasks.archive(task.id));
      await fetchTasks();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [task, fetchTasks, intl]);

  const togglePanel = useCallback(() => {
    setSheetOpen(true); // mobile: open the sheet
    toggleCollapsed(); // desktop: toggle the 320px column
  }, [setSheetOpen, toggleCollapsed]);

  // ── Loading / not-found ──────────────────────────────────
  if (!task) {
    return (
      <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
        <BreadcrumbHeader
          hideTrigger
          segments={[{ label: intl.formatMessage({ id: 'nav.tasks' }), onClick: () => navigate('/tasks') }]}
        />
        {error != null && !loading ? (
          // "Couldn't load" and "this task doesn't exist" are different answers
          // and used to share one screen.
          <ErrorState
            icon={ClipboardList}
            error={error}
            onRetry={() => {
              clearError();
              void fetchTasks();
            }}
            action={
              <Button variant="outline" size="sm" onClick={() => navigate('/tasks')}>
                <ArrowLeft />
                {intl.formatMessage({ id: 'tasks.detail.backToBoard' })}
              </Button>
            }
          />
        ) : (
          <Empty
            icon={ClipboardList}
            title={intl.formatMessage({ id: loading ? 'common.loading' : 'tasks.detail.notFound' })}
            action={
              <Button variant="outline" size="sm" onClick={() => navigate('/tasks')}>
                <ArrowLeft />
                {intl.formatMessage({ id: 'tasks.detail.backToBoard' })}
              </Button>
            }
          />
        )}
      </div>
    );
  }

  const source = taskSource(task);
  const isDone = task.status === 'done';
  // The Live pill (label "進行中" — same text as taskStatus.in_progress) marks an
  // in-flight run. Gate it on the task NOT being done so it never lingers as a
  // stale "進行中" next to a task that's already complete (#4).
  const showLive = assigneeAgent?.status === 'active' && !isDone;

  const segments: BreadcrumbSegment[] = [
    { label: intl.formatMessage({ id: 'nav.tasks' }), onClick: () => navigate('/tasks') },
    { label: `${task.id.slice(0, 8)} · ${task.title}` },
  ];

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <BreadcrumbHeader
        hideTrigger
        segments={segments}
        actions={
          <>
            {/* Hidden while the task waits on a decision — the panel below is
                the single writer for that state (WP-A §2-6). */}
            {!needsHuman && (
              <Button
                variant="ghost"
                size="icon-sm"
                onClick={() => applyStatus(isDone ? 'todo' : 'done')}
                aria-label={intl.formatMessage({ id: isDone ? 'tasks.detail.markDoneUndo' : 'tasks.detail.markDone' })}
                title={intl.formatMessage({ id: isDone ? 'tasks.detail.markDoneUndo' : 'tasks.detail.markDone' })}
              >
                <CircleCheck className={isDone ? 'text-success' : undefined} />
              </Button>
            )}
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button variant="ghost" size="icon-sm" aria-label={intl.formatMessage({ id: 'tasks.detail.more' })} />
                }
              >
                <MoreHorizontal />
              </DropdownMenuTrigger>
              <DropdownMenuContent>
                <DropdownMenuItem onClick={copyLink}>
                  <Link2 />
                  {intl.formatMessage({ id: 'tasks.detail.copyLink' })}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => void togglePin()}>
                  {task.pinned ? <PinOff /> : <Pin />}
                  {intl.formatMessage({ id: task.pinned ? 'goals.action.unpin' : 'goals.action.pin' })}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => void toggleArchive()}>
                  {task.archived ? <ArchiveRestore /> : <Archive />}
                  {intl.formatMessage({ id: task.archived ? 'goals.action.unarchive' : 'goals.action.archive' })}
                </DropdownMenuItem>
                <DropdownMenuItem variant="destructive" onClick={() => setConfirmRemove(true)}>
                  <Trash2 />
                  {intl.formatMessage({ id: 'tasks.remove' })}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={togglePanel}
              aria-label={intl.formatMessage({ id: 'tasks.detail.toggleProps' })}
              title={intl.formatMessage({ id: 'tasks.detail.toggleProps' })}
            >
              <PanelRight />
            </Button>
          </>
        }
      />

      {/* Left content column (spec §5.3 式1: mx-auto max-w-4xl px-8 py-8). */}
      <div className="mx-auto w-full max-w-4xl space-y-6 px-5 py-6 md:px-8 md:py-8">
        {error != null && (
          <ErrorState
            variant="inline"
            title={intl.formatMessage({ id: 'errorState.actionFailed' })}
            error={error}
            onRetry={() => {
              clearError();
              void fetchTasks();
            }}
          />
        )}

        {/* Parent chain */}
        {parent && (
          <button
            type="button"
            onClick={() => navigate(`/tasks/${parent.id}`)}
            className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" />
            <span className="truncate">{parent.title}</span>
          </button>
        )}

        {/* Title (inline edit) */}
        <div className="space-y-2">
          <InlineEditor
            value={task.title}
            onCommit={(next) => updateTask(task.id, { title: next })}
            ariaLabel={intl.formatMessage({ id: 'tasks.field.title' })}
            textClassName="text-2xl font-bold tracking-tight text-foreground"
          />
          {/* Meta row: status glyph + source + live */}
          <div className="flex flex-wrap items-center gap-2 px-1.5">
            <StatusIcon status={toStatusKey(task.status)} size="sm" />
            <Badge variant="secondary">{intl.formatMessage({ id: `tasks.source.${source}` })}</Badge>
            <GoalRoundBadge task={task} />
            <GoalDiminishingBadge task={task} />
            <GoalDeadlineBadge task={task} />
            {showLive && <LiveBadge />}
            {assigneeAgent && (
              <span className="ml-1 inline-flex items-center gap-1.5" title={assigneeAgent.display_name}>
                <ActorAvatar actorType="agent" size="sm" name={assigneeAgent.display_name} />
                <span className="text-xs text-muted-foreground">{assigneeAgent.display_name}</span>
              </span>
            )}
            {/* W2-3 reverse handoff (E8): jump back to the /goal conversation. */}
            <OpenInChannelButton channel={task.channel} link={task.channel_link} className="ml-auto" />
          </div>
        </div>

        {/* I-2c: a goal task can also be parked awaiting its FIRST dispatch
            (collaborator/consultant autonomy kickoff gate) — a distinct
            human node from `needs_human`, so it renders independently of the
            section below (the two never overlap on one task). */}
        {task.goal_mode && (
          <GoalPendingKickoff pendingKickoff={timeline?.pending_kickoff ?? null} onDecided={loadTimeline} />
        )}

        {/* WP-A (§2-6): the one decision surface for a 等你決定 task, identical to
            the inbox's — reason first, then the same three choices.
            I-1c: a task parked with `plan_pending` came from 想一想 mode —
            same three buttons (重試 = 核准並開始執行 / 放棄 = 取消任務), but a
            distinct heading + a dedicated plan card instead of the generic
            「卡住原因」 line (which would otherwise duplicate the same text,
            since the server also mirrors the plan into `judge_feedback` for
            channel/legacy display surfaces).
            I-2c: goal-mode tasks additionally get the H11 pause-reason chip
            (why it stopped, not just the free-text feedback) and a fourth
            決策 — 「交給我」take-over — next to the shared 3-button
            `NeedsHumanActions` (kept unchanged so a non-goal `blocked` task's
            decision flow is not lengthened). */}
        {needsHuman && (
          <section
            aria-label={intl.formatMessage({ id: 'taskStatus.needs_human' })}
            className="space-y-3 rounded-xl border border-amber-500/30 bg-amber-500/10 p-4"
          >
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-sm font-medium text-foreground">
                {intl.formatMessage({
                  id: task.plan_pending ? 'tasks.planFirst.prompt' : 'tasks.needsHuman.prompt',
                })}
              </p>
              {task.goal_mode && <PauseReasonChip reason={task.pause_reason} />}
            </div>
            {task.plan_pending ? (
              <div className="space-y-1.5 rounded-lg bg-surface px-3 py-2.5">
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'tasks.planFirst.hint' })}
                </p>
                <p className="whitespace-pre-wrap text-sm text-foreground">{task.plan_pending}</p>
              </div>
            ) : (
              task.judge_feedback && (
                <p className="flex items-start gap-2 rounded-lg bg-surface px-3 py-2 text-xs text-muted-foreground">
                  <MessageSquareWarning className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                  <span>{task.judge_feedback}</span>
                </p>
              )
            )}
            <div className="flex flex-wrap items-center gap-2">
              <NeedsHumanActions taskId={task.id} onResolved={fetchTasks} />
              {task.goal_mode && <GoalTakeoverButton taskId={task.id} onDecided={fetchTasks} />}
            </div>
          </section>
        )}

        {/* I-3a: 已完成／失敗／已放棄的目標任務可以「接著做」。 */}
        {canGoalContinue && <GoalContinuePanel task={task} onContinued={fetchTasks} />}

        {/* I-2c: goal contract — risk boundary / acceptance criteria / latest
            result / confirmed facts. Renders nothing for a plain board task
            or a goal task with no contract fields set. */}
        <GoalContractCards task={task} />

        {/* Description (inline edit, multiline) */}
        <div>
          <h2 className="mb-1 px-1.5 text-xs font-medium text-muted-foreground">
            {intl.formatMessage({ id: 'tasks.field.description' })}
          </h2>
          <InlineEditor
            value={task.description}
            onCommit={(next) => updateTask(task.id, { description: next })}
            multiline
            placeholder={intl.formatMessage({ id: 'tasks.detail.noDescription' })}
            ariaLabel={intl.formatMessage({ id: 'tasks.field.description' })}
            textClassName="whitespace-pre-wrap text-sm text-foreground/90"
          />
        </div>

        {/* Subtasks (real: derived from parent_task_id) */}
        <div>
          <div className="mb-1.5 flex items-center justify-between px-1.5">
            <h2 className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'tasks.subtasks' })}
              {subtasks.length > 0 && (
                <span className="ml-1.5 font-mono tabular-nums text-muted-foreground">
                  {subtasks.filter((s) => s.status === 'done').length}/{subtasks.length}
                </span>
              )}
            </h2>
            <Button variant="ghost" size="sm" onClick={() => setAddSub(true)}>
              <Plus />
              {intl.formatMessage({ id: 'tasks.subtask.add' })}
            </Button>
          </div>
          {subtasks.length > 0 ? (
            <ul className="overflow-hidden rounded-xl border border-surface-border bg-surface">
              {subtasks.map((s) => {
                const a = agents.find((ag) => ag.name === s.assigned_to);
                return (
                  <li
                    key={s.id}
                    onClick={() => navigate(`/tasks/${s.id}`)}
                    className="flex cursor-pointer items-center gap-2.5 px-4 py-2 transition-colors hover:bg-surface-hover"
                  >
                    <StatusIcon status={toStatusKey(s.status)} size="sm" />
                    <span className="min-w-0 flex-1 truncate text-sm text-foreground">{s.title}</span>
                    {a && <ActorAvatar actorType="agent" size="sm" name={a.display_name} />}
                    <ChevronRight className="size-3.5 shrink-0 text-muted-foreground/60" />
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="px-1.5 text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'tasks.subtasks.empty' })}
            </p>
          )}
        </div>

        {/* Iterative Kanban: dual clock + revision timeline (goal-mode only).
            I-2c: the round-by-round list itself is now `GoalRoundTimeline`,
            shared with the (now-removed) `/goals?task=` dialog — it carries
            everything that page's version had (per-aspect MAV verdicts,
            redispatch/repeat-state flags, run links, merged loop activity
            events, the foresight link) plus this page's own dispatched/
            submitted/judged stage breakdown, so neither side lost anything
            in the merge. */}
        {(iterations.length > 0 || (task.agent_seconds ?? 0) > 0) && (
          <div>
            <h2 className="mb-2 px-1.5 text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'tasks.iterations.title' })}
            </h2>

            {/* Dual clock: agent processing time vs full human+machine cycle. */}
            <div className="mb-3 grid grid-cols-2 gap-2 px-1.5">
              <div className="rounded-lg border border-surface-border bg-surface px-3 py-2">
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'tasks.clock.agent' })}
                </p>
                <p className="font-mono text-sm font-semibold tabular-nums text-foreground">
                  {formatHms(task.agent_seconds ?? 0)}
                </p>
              </div>
              <div className="rounded-lg border border-surface-border bg-surface px-3 py-2">
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'tasks.clock.cycle' })}
                </p>
                <p className="font-mono text-sm font-semibold tabular-nums text-foreground">
                  {formatHms(secondsBetween(task.created_at, task.completed_at || task.updated_at))}
                </p>
              </div>
            </div>

            <GoalRoundTimeline task={task} timeline={timeline} />
          </div>
        )}

        {/* Updated timestamp byline */}
        <p className="px-1.5 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'tasks.detail.updatedAgo' })}{' '}
          <span className="font-mono tabular-nums">{timeAgo(task.updated_at)}</span>
        </p>

        {/* Bottom tabs — I-2a 產物／檔案／變更／過程 */}
        <TaskBottomTabs
          taskId={task.id}
          agentId={task.assigned_to}
          events={taskActivities}
          comments={taskComments}
          agents={agents}
          onAddComment={async (body) => {
            clearError();
            const added = await addComment(task.id, body);
            if (!added) toast.error(intl.formatMessage({ id: 'tasks.comment.failed' }));
          }}
          currentUserId={currentUser?.id}
          currentUserName={currentUser?.display_name}
        />
      </div>

      {/* Add-subtask modal (reuses the create modal with parentTaskId) */}
      <CreateTaskModal
        open={addSub}
        onClose={() => setAddSub(false)}
        agents={agents}
        onCreate={createTask}
        parentTaskId={task.id}
        defaultAssignee={task.assigned_to}
      />

      {/* Delete confirmation */}
      <Dialog open={confirmRemove} onOpenChange={setConfirmRemove}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'tasks.remove' })}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage({ id: 'tasks.remove.confirm' }, { title: task.title })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'agents.delegate.close' })}</Button>}
            />
            <Button variant="destructive" onClick={handleRemove}>
              {intl.formatMessage({ id: 'tasks.remove' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {burst && (
        <TaskDoneBurst agentId={burst.agentId} agentName={assigneeAgent?.display_name} onDone={() => setBurst(null)} />
      )}
    </div>
  );
}

/**
 * GoalContinuePanel — I-3a "接著做": a done/failed/cancelled goal task can
 * take a follow-up message and get reopened for another round instead of
 * staying a dead end (`DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.3,
 * backlog item I-3a). The message is required — continuing "with nothing to
 * add" is what `NeedsHumanActions`' 重試 already does for `needs_human`.
 * Backed by `tasks.goal_decide` with `action: "continue"`
 * (`api.tasks.goalContinue`), which reuses the acceptance criteria and risk
 * boundary already on the task and carries the message into the next
 * dispatch's prompt as a follow-up instruction — the reopened round still
 * goes through the same MAV judge as any other round (the hint copy below
 * says so explicitly, since WorkBuddy's equivalent skips review entirely).
 */
function GoalContinuePanel({ task, onContinued }: { task: TaskInfo; onContinued: () => void }) {
  const intl = useIntl();
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);

  const submit = useCallback(async () => {
    const trimmed = message.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      const r = await api.tasks.goalContinue(task.id, trimmed);
      toast.success(r.message);
      setMessage('');
      onContinued();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  }, [message, busy, task.id, intl, onContinued]);

  return (
    <section
      aria-label={intl.formatMessage({ id: 'tasks.continue.title' })}
      className="space-y-2.5 rounded-xl border border-surface-border bg-muted/40 p-4"
    >
      <p className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'tasks.continue.title' })}</p>
      <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'tasks.continue.hint' })}</p>
      <Textarea
        value={message}
        onChange={(e) => setMessage(e.target.value)}
        placeholder={intl.formatMessage({ id: 'tasks.continue.placeholder' })}
        disabled={busy}
        rows={2}
        aria-label={intl.formatMessage({ id: 'tasks.continue.title' })}
      />
      <div className="flex justify-end">
        <Button variant="brand" size="sm" disabled={busy || !message.trim()} onClick={submit}>
          {busy ? <Loader2 className="animate-spin" /> : <RotateCcw />}
          {intl.formatMessage({ id: 'tasks.continue.submit' })}
        </Button>
      </div>
    </section>
  );
}
