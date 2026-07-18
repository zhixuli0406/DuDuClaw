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
  type BreadcrumbSegment,
} from '@/components/mds';
import { StatusIcon, InlineEditor, LiveBadge, usePanel } from '@/components/ui';
import {
  CreateTaskModal,
  TaskProperties,
  TaskBottomTabs,
  TaskDoneBurst,
  celebrateTaskDone,
} from '@/components/task';
import { toStatusKey } from '@/lib/task-status';
import { timeAgo } from '@/lib/format';
import type { TaskInfo, TaskStatus, TaskPriority } from '@/lib/api';
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
} from 'lucide-react';
import { toast } from '@/lib/toast';

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
  const {
    tasks,
    activities,
    comments,
    loading,
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

  useEffect(() => {
    fetchTasks();
    fetchAgents();
    fetchActivities({ limit: 100 });
  }, [fetchTasks, fetchAgents, fetchActivities]);

  useEffect(() => {
    if (id) fetchComments(id);
  }, [id, fetchComments]);

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
          onStatusChange={applyStatus}
          onPriorityChange={applyPriority}
          onAssign={applyAssign}
        />
      ),
    });
  }, [task, agents, setPanel, intl, applyStatus, applyPriority, applyAssign]);

  const copyLink = useCallback(() => {
    try {
      void navigator.clipboard?.writeText(window.location.href);
      toast.success(intl.formatMessage({ id: 'tasks.detail.linkCopied' }));
    } catch {
      /* clipboard blocked — silent */
    }
  }, [intl]);

  const handleRemove = useCallback(async () => {
    if (task) {
      await removeTask(task.id);
      setConfirmRemove(false);
      navigate('/tasks');
    }
  }, [task, removeTask, navigate]);

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
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => applyStatus(isDone ? 'todo' : 'done')}
              aria-label={intl.formatMessage({ id: isDone ? 'tasks.detail.markDoneUndo' : 'tasks.detail.markDone' })}
              title={intl.formatMessage({ id: isDone ? 'tasks.detail.markDoneUndo' : 'tasks.detail.markDone' })}
            >
              <CircleCheck className={isDone ? 'text-success' : undefined} />
            </Button>
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
            {showLive && <LiveBadge />}
            {assigneeAgent && (
              <span className="ml-1 inline-flex items-center gap-1.5" title={assigneeAgent.display_name}>
                <ActorAvatar actorType="agent" size="sm" name={assigneeAgent.display_name} />
                <span className="text-xs text-muted-foreground">{assigneeAgent.display_name}</span>
              </span>
            )}
          </div>
        </div>

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

        {/* Updated timestamp byline */}
        <p className="px-1.5 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'tasks.detail.updatedAgo' })}{' '}
          <span className="font-mono tabular-nums">{timeAgo(task.updated_at)}</span>
        </p>

        {/* Bottom tabs (discussion / activity) */}
        <TaskBottomTabs
          events={taskActivities}
          comments={taskComments}
          agents={agents}
          onAddComment={async (body) => {
            await addComment(task.id, body);
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
