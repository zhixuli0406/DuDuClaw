import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams } from 'react-router';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { Dialog } from '@/components/shared/Dialog';
import {
  Page,
  Card,
  Button,
  Badge,
  EmptyState,
  Mono,
  StatusIcon,
  PriorityIcon,
  LiveBadge,
  InlineEditor,
  CharacterAvatar,
  usePanel,
} from '@/components/ui';
import {
  CreateTaskModal,
  TaskProperties,
  TaskBottomTabs,
  TaskDoneBurst,
  celebrateTaskDone,
} from '@/components/task';
import { toStatusKey, toBackendStatus } from '@/lib/task-status';
import { timeAgo } from '@/lib/format';
import type { TaskInfo, TaskStatus, TaskPriority } from '@/lib/api';
import {
  ArrowLeft,
  Link2,
  SlidersHorizontal,
  MoreHorizontal,
  Trash2,
  Plus,
  ClipboardList,
  ChevronRight,
} from 'lucide-react';
import { toast } from '@/lib/toast';

type TaskSource = 'channel' | 'delegated' | 'manual';
function taskSource(task: TaskInfo): TaskSource {
  if (task.message_id) return 'channel';
  if (task.parent_task_id) return 'delegated';
  return 'manual';
}

/** `/tasks/:id` — the paperclip IssueDetail flagship (§5.3 T5.2–T5.5). */
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
  const [menuOpen, setMenuOpen] = useState(false);
  const [burst, setBurst] = useState<{ agentId: string } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    fetchTasks();
    fetchAgents();
    fetchActivities({ limit: 100 });
  }, [fetchTasks, fetchAgents, fetchActivities]);

  // Load this task's comments once we know the id.
  useEffect(() => {
    if (id) fetchComments(id);
  }, [id, fetchComments]);

  const task = useMemo(() => tasks.find((t) => t.id === id), [tasks, id]);
  const parent = useMemo(
    () => (task?.parent_task_id ? tasks.find((t) => t.id === task.parent_task_id) : undefined),
    [tasks, task?.parent_task_id],
  );
  // Subtasks are derived from the full task list (tasks.list is unfiltered).
  // TaskInfo carries `parent_task_id`, so this is real data, not a stand-in.
  const subtasks = useMemo(() => (id ? tasks.filter((t) => t.parent_task_id === id) : []), [tasks, id]);
  const taskActivities = useMemo(
    () => activities.filter((e) => e.task_id === id),
    [activities, id],
  );
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

  // ── Right-hand PropertiesPanel (§5.3 T5.3) ───────────────
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

  // Close the ⋯ menu on outside click.
  useEffect(() => {
    if (!menuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [menuOpen]);

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

  // ── Loading / not-found ──────────────────────────────────
  if (!task) {
    return (
      <Page>
        <Card padded={false}>
          <EmptyState
            icon={ClipboardList}
            title={intl.formatMessage({ id: loading ? 'common.loading' : 'tasks.detail.notFound' })}
            action={
              <Button variant="secondary" icon={ArrowLeft} onClick={() => navigate('/tasks')}>
                {intl.formatMessage({ id: 'tasks.detail.backToBoard' })}
              </Button>
            }
          />
        </Card>
      </Page>
    );
  }

  const source = taskSource(task);
  const showLive = assigneeAgent?.status === 'active';

  return (
    <Page>
      <div className="mx-auto w-full max-w-3xl space-y-6">
        {/* 1 · parent chain */}
        {parent && (
          <button
            type="button"
            onClick={() => navigate(`/tasks/${parent.id}`)}
            className="flex items-center gap-1 text-sm text-stone-500 hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            <span className="truncate">{parent.title}</span>
          </button>
        )}

        {/* 2 · status row */}
        <div className="flex flex-wrap items-center gap-2">
          <StatusIcon
            status={toStatusKey(task.status)}
            size="md"
            onChange={(key) => {
              const backend = toBackendStatus(key);
              if (backend) applyStatus(backend);
            }}
          />
          <PriorityIcon priority={task.priority} size="md" />
          <Mono>{task.id.slice(0, 8)}</Mono>
          {showLive && <LiveBadge />}
          <Badge tone="neutral">{intl.formatMessage({ id: `tasks.source.${source}` })}</Badge>
          {assigneeAgent && (
            <span className="ml-1 flex items-center gap-1.5" title={assigneeAgent.display_name}>
              <CharacterAvatar agentId={assigneeAgent.name} name={assigneeAgent.display_name} size={22} animated={false} />
              <span className="text-xs text-stone-500 dark:text-stone-400">{assigneeAgent.display_name}</span>
            </span>
          )}

          <div className="ml-auto flex items-center gap-1">
            <Button variant="ghost" size="sm" icon={Link2} onClick={copyLink} title={intl.formatMessage({ id: 'tasks.detail.copyLink' })} />
            <Button
              variant="ghost"
              size="sm"
              icon={SlidersHorizontal}
              title={intl.formatMessage({ id: 'tasks.props.title' })}
              onClick={() => {
                setSheetOpen(true); // mobile: open the bottom sheet
                toggleCollapsed(); // desktop: toggle the 320px column
              }}
            />
            <div ref={menuRef} className="relative">
              <Button
                variant="ghost"
                size="sm"
                icon={MoreHorizontal}
                aria-haspopup="menu"
                aria-expanded={menuOpen}
                onClick={() => setMenuOpen((v) => !v)}
              />
              {menuOpen && (
                <div role="menu" className="glass-overlay absolute right-0 top-full z-50 mt-1 min-w-40 rounded-control p-1">
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setMenuOpen(false);
                      setConfirmRemove(true);
                    }}
                    className="flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm text-rose-600 hover:bg-rose-500/10 dark:text-rose-400"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'tasks.remove' })}
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>

        {/* 3 · title (inline edit) */}
        <InlineEditor
          value={task.title}
          onCommit={(next) => updateTask(task.id, { title: next })}
          ariaLabel={intl.formatMessage({ id: 'tasks.field.title' })}
          textClassName="text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50"
        />

        {/* 4 · description (inline edit, multiline) */}
        <div>
          <h2 className="mb-1 px-1.5 text-xs font-semibold uppercase tracking-wide text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'tasks.field.description' })}
          </h2>
          <InlineEditor
            value={task.description}
            onCommit={(next) => updateTask(task.id, { description: next })}
            multiline
            placeholder={intl.formatMessage({ id: 'tasks.detail.noDescription' })}
            ariaLabel={intl.formatMessage({ id: 'tasks.field.description' })}
            textClassName="whitespace-pre-wrap text-sm text-stone-700 dark:text-stone-300"
          />
        </div>

        {/* 5 · subtasks (real: derived from parent_task_id) */}
        <div>
          <div className="mb-1.5 flex items-center justify-between px-1.5">
            <h2 className="text-xs font-semibold uppercase tracking-wide text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'tasks.subtasks' })}
              {subtasks.length > 0 && (
                <span className="ml-1.5 tabular-nums text-stone-400 dark:text-stone-500">
                  {subtasks.filter((s) => s.status === 'done').length}/{subtasks.length}
                </span>
              )}
            </h2>
            <Button variant="ghost" size="sm" icon={Plus} onClick={() => setAddSub(true)}>
              {intl.formatMessage({ id: 'tasks.subtask.add' })}
            </Button>
          </div>
          {subtasks.length > 0 ? (
            <Card padded={false}>
              <ul className="divide-y divide-stone-200/60 dark:divide-white/5">
                {subtasks.map((s) => {
                  const a = agents.find((ag) => ag.name === s.assigned_to);
                  return (
                    <li key={s.id} className="flex items-center gap-2.5 px-4 py-2 hover:bg-stone-500/5 dark:hover:bg-white/5">
                      <StatusIcon status={toStatusKey(s.status)} size="sm" />
                      <button
                        type="button"
                        onClick={() => navigate(`/tasks/${s.id}`)}
                        className="min-w-0 flex-1 truncate text-left text-sm text-stone-700 hover:text-amber-600 dark:text-stone-200 dark:hover:text-amber-400"
                      >
                        {s.title}
                      </button>
                      {a && <CharacterAvatar agentId={a.name} name={a.display_name} size={20} animated={false} />}
                      <ChevronRight className="h-3.5 w-3.5 shrink-0 text-stone-300 dark:text-stone-600" />
                    </li>
                  );
                })}
              </ul>
            </Card>
          ) : (
            <p className="px-1.5 text-sm text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'tasks.subtasks.empty' })}
            </p>
          )}
        </div>

        {/* 6 · updated timestamp byline */}
        <p className="px-1.5 text-xs text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'tasks.detail.updatedAgo' })} <Mono className="text-[0.6875rem]">{timeAgo(task.updated_at)}</Mono>
        </p>

        {/* 7 · bottom tabs (discussion / activity) */}
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
      <Dialog open={confirmRemove} title={intl.formatMessage({ id: 'tasks.remove' })} onClose={() => setConfirmRemove(false)}>
        <div className="space-y-4">
          <p className="text-sm text-stone-600 dark:text-stone-400">
            {intl.formatMessage({ id: 'tasks.remove.confirm' }, { title: task.title })}
          </p>
          <div className="flex justify-end gap-3">
            <Button variant="secondary" onClick={() => setConfirmRemove(false)}>
              {intl.formatMessage({ id: 'agents.delegate.close' })}
            </Button>
            <Button variant="danger" onClick={handleRemove}>
              {intl.formatMessage({ id: 'tasks.remove' })}
            </Button>
          </div>
        </div>
      </Dialog>

      {burst && (
        <TaskDoneBurst agentId={burst.agentId} agentName={assigneeAgent?.display_name} onDone={() => setBurst(null)} />
      )}
    </Page>
  );
}
