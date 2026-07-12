import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import { Dialog } from '@/components/shared/Dialog';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  Button,
  EmptyState,
  GroupHeader,
  StatusIcon,
  PriorityIcon,
  CharacterAvatar,
  Mono,
  controlClass,
} from '@/components/ui';
import { CreateTaskModal, TaskDoneBurst, celebrateTaskDone } from '@/components/task';
import { toStatusKey, toBackendStatus } from '@/lib/task-status';
import { timeAgo } from '@/lib/format';
import type { TaskInfo, TaskStatus, TaskPriority, TaskCreateParams } from '@/lib/api';
import {
  Plus,
  GripVertical,
  Clock,
  AlertCircle,
  CheckCircle2,
  Ban,
  Filter,
  Trash2,
  KanbanSquare,
  List,
} from 'lucide-react';

// ── Preferences (localStorage, §4.4) ────────────────────────
const VIEW_KEY = 'duduclaw:tasks:view';
const GROUP_KEY = 'duduclaw:tasks:group';
// Persisted set of collapsed group/swimlane keys (shared by the list group-by
// and the kanban by-agent swimlanes, both keyed by assignee).
const COLLAPSE_KEY = 'duduclaw:tasks:collapsed';
// When grouping by AI staff, auto-collapse the swimlanes once there are this
// many buckets — so a large roster doesn't unfurl into a wall of boards.
const MANY_AGENTS = 4;
type ViewMode = 'kanban' | 'list';
type GroupMode = 'status' | 'assignee';

function readPref<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v && (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
  } catch {
    return fallback;
  }
}
function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private mode — preference just won't persist */
  }
}

function readCollapsed(): Set<string> {
  try {
    const raw = localStorage.getItem(COLLAPSE_KEY);
    if (raw) {
      const arr = JSON.parse(raw);
      if (Array.isArray(arr)) return new Set(arr.filter((v): v is string => typeof v === 'string'));
    }
  } catch {
    /* ignore malformed / unavailable storage */
  }
  return new Set();
}
function writeCollapsed(s: ReadonlySet<string>) {
  try {
    localStorage.setItem(COLLAPSE_KEY, JSON.stringify([...s]));
  } catch {
    /* private mode — preference just won't persist */
  }
}

const COLUMNS: ReadonlyArray<{ status: TaskStatus; icon: React.ComponentType<{ className?: string }> }> = [
  { status: 'todo', icon: Clock },
  { status: 'in_progress', icon: AlertCircle },
  { status: 'done', icon: CheckCircle2 },
  { status: 'blocked', icon: Ban },
];

const COLUMN_STYLES: Record<TaskStatus, string> = {
  todo: 'border-t-stone-400',
  in_progress: 'border-t-amber-500',
  done: 'border-t-emerald-500',
  blocked: 'border-t-rose-500',
};

const PRIORITY_TONES: Record<TaskPriority, 'neutral' | 'info' | 'warning' | 'danger'> = {
  low: 'neutral',
  medium: 'info',
  high: 'warning',
  urgent: 'danger',
};

// ── Board card ──────────────────────────────────────────────
function TaskCard({
  task,
  agent,
  onOpen,
  onRemove,
}: {
  task: TaskInfo;
  agent?: { name: string; display_name: string };
  onOpen: (id: string) => void;
  onRemove: (task: TaskInfo) => void;
}) {
  const intl = useIntl();
  const handleDragStart = useCallback(
    (e: DragEvent<HTMLDivElement>) => {
      e.dataTransfer.setData('text/plain', task.id);
      e.dataTransfer.effectAllowed = 'move';
    },
    [task.id],
  );

  return (
    <div
      draggable
      onDragStart={handleDragStart}
      className="panel panel-hover group cursor-grab rounded-card p-3 active:cursor-grabbing"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="flex min-w-0 items-start gap-2">
          <GripVertical className="mt-0.5 h-4 w-4 flex-shrink-0 text-stone-300 dark:text-stone-600" />
          <div className="min-w-0">
            <button
              onClick={() => onOpen(task.id)}
              className="text-left text-sm font-medium text-stone-900 hover:text-amber-600 dark:text-stone-50 dark:hover:text-amber-400"
            >
              {task.title}
            </button>
            {task.description && (
              <p className="mt-1 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">{task.description}</p>
            )}
          </div>
        </div>
        <button
          onClick={() => onRemove(task)}
          className="flex-shrink-0 opacity-0 transition-opacity group-hover:opacity-100"
          title={intl.formatMessage({ id: 'tasks.remove' })}
        >
          <Trash2 className="h-3.5 w-3.5 text-stone-400 hover:text-rose-500" />
        </button>
      </div>

      <div className="mt-3 flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          <PriorityIcon priority={task.priority} size="sm" />
          <Badge tone={PRIORITY_TONES[task.priority]}>
            {intl.formatMessage({ id: `tasks.priority.${task.priority}` })}
          </Badge>
        </div>
        {agent && (
          <span className="flex items-center gap-1.5" title={agent.display_name}>
            <CharacterAvatar agentId={agent.name} name={agent.display_name} size={22} animated={false} />
            <span className="max-w-[84px] truncate text-xs text-stone-500 dark:text-stone-400">
              {agent.display_name}
            </span>
          </span>
        )}
      </div>

      {task.status === 'blocked' && task.blocked_reason && (
        <div className="mt-2 rounded-md bg-rose-50 px-2 py-1 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
          {task.blocked_reason}
        </div>
      )}
    </div>
  );
}

// ── Kanban column ───────────────────────────────────────────
function KanbanColumn({
  status,
  icon: Icon,
  tasks,
  agents,
  onDrop,
  onOpen,
  onRemove,
}: {
  status: TaskStatus;
  icon: React.ComponentType<{ className?: string }>;
  tasks: ReadonlyArray<TaskInfo>;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  onDrop: (taskId: string, status: TaskStatus) => void;
  onOpen: (id: string) => void;
  onRemove: (task: TaskInfo) => void;
}) {
  const intl = useIntl();
  const [isDragOver, setIsDragOver] = useState(false);

  return (
    <div
      className={cn('panel flex min-h-[300px] flex-col border-t-4', COLUMN_STYLES[status], isDragOver && 'ring-2 ring-amber-400/50')}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        setIsDragOver(true);
      }}
      onDragLeave={() => setIsDragOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setIsDragOver(false);
        const taskId = e.dataTransfer.getData('text/plain');
        if (taskId) onDrop(taskId, status);
      }}
    >
      <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-4 py-3">
        <div className="flex items-center gap-2">
          <Icon className="h-4 w-4 text-stone-500 dark:text-stone-400" />
          <h3 className="text-sm font-semibold text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: `tasks.column.${status}` })}
          </h3>
          <Badge tone="neutral" className="tabular-nums">
            {tasks.length}
          </Badge>
        </div>
      </div>

      <div className="flex-1 space-y-2 p-3">
        {tasks.map((task) => (
          <TaskCard
            key={task.id}
            task={task}
            agent={agents.find((a) => a.name === task.assigned_to)}
            onOpen={onOpen}
            onRemove={onRemove}
          />
        ))}
        {tasks.length === 0 && (
          <div
            className={cn(
              'flex items-center justify-center rounded-card border-2 border-dashed py-8 text-xs text-stone-400 transition-colors dark:text-stone-600',
              isDragOver ? 'border-amber-400 bg-amber-50/50 dark:bg-amber-900/10' : 'border-[var(--panel-border)]',
            )}
          >
            {intl.formatMessage({ id: 'tasks.dropHint' })}
          </div>
        )}
      </div>
    </div>
  );
}

// ── List row ────────────────────────────────────────────────
function TaskListRow({
  task,
  agent,
  onOpen,
  onStatus,
}: {
  task: TaskInfo;
  agent?: { name: string; display_name: string };
  onOpen: (id: string) => void;
  onStatus: (task: TaskInfo, key: import('@/components/ui').TaskStatusKey) => void;
}) {
  return (
    <li className="flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-stone-500/5 dark:hover:bg-white/5">
      <StatusIcon status={toStatusKey(task.status)} size="sm" onChange={(key) => onStatus(task, key)} />
      <button
        type="button"
        onClick={() => onOpen(task.id)}
        className="min-w-0 flex-1 truncate text-left text-sm font-medium text-stone-800 hover:text-amber-600 dark:text-stone-100 dark:hover:text-amber-400"
      >
        {task.title}
      </button>
      <PriorityIcon priority={task.priority} size="sm" />
      {agent && (
        <span className="hidden items-center gap-1.5 sm:flex" title={agent.display_name}>
          <CharacterAvatar agentId={agent.name} name={agent.display_name} size={24} animated={false} />
          <span className="max-w-[100px] truncate text-xs text-stone-500 dark:text-stone-400">{agent.display_name}</span>
        </span>
      )}
      <Mono className="hidden w-16 shrink-0 text-right text-[0.6875rem] md:inline">{task.id.slice(0, 8)}</Mono>
      <span className="w-12 shrink-0 text-right text-[0.6875rem] tabular-nums text-stone-400 dark:text-stone-500">
        {timeAgo(task.updated_at)}
      </span>
    </li>
  );
}

// ── Page ────────────────────────────────────────────────────
export function TaskBoardPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const {
    tasks,
    loading,
    fetchTasks,
    createTask,
    moveTask,
    removeTask,
    filterAgent,
    filterPriority,
    setFilterAgent,
    setFilterPriority,
  } = useTasksStore();
  const { agents, fetchAgents } = useAgentsStore();

  const [view, setView] = useState<ViewMode>(() => readPref(VIEW_KEY, ['kanban', 'list'] as const, 'kanban'));
  const [group, setGroup] = useState<GroupMode>(() => readPref(GROUP_KEY, ['status', 'assignee'] as const, 'status'));
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(readCollapsed);
  const [showCreate, setShowCreate] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<TaskInfo | null>(null);
  const [burst, setBurst] = useState<{ agentId: string } | null>(null);

  useEffect(() => {
    fetchTasks();
    fetchAgents();
  }, [fetchTasks, fetchAgents]);

  // Open the create modal when routed here with `?new=1` (Sidebar / MobileBottomNav).
  useEffect(() => {
    if (searchParams.get('new') === '1') setShowCreate(true);
  }, [searchParams]);

  // `?assignee=<id>` (W3b employee-detail "交辦" button) preselects the assignee
  // in the create modal (A3). Read live so a fresh deep-link seeds the picker.
  const defaultAssignee = searchParams.get('assignee') || undefined;

  const setViewPref = (v: ViewMode) => {
    setView(v);
    writePref(VIEW_KEY, v);
  };
  const setGroupPref = (g: GroupMode) => {
    setGroup(g);
    writePref(GROUP_KEY, g);
  };

  const closeCreate = useCallback(() => {
    setShowCreate(false);
    if (searchParams.get('new') || searchParams.get('assignee')) {
      const next = new URLSearchParams(searchParams);
      next.delete('new');
      next.delete('assignee');
      setSearchParams(next, { replace: true });
    }
  }, [searchParams, setSearchParams]);

  const openTask = useCallback((id: string) => navigate(`/tasks/${id}`), [navigate]);

  const handleCreate = useCallback(async (params: TaskCreateParams) => createTask(params), [createTask]);

  // One completion path for both drag-drop and the list StatusIcon: write via
  // the store, and fire the §5.5 celebration when a task first reaches `done`.
  const applyStatus = useCallback(
    (task: TaskInfo, next: TaskStatus) => {
      if (next === task.status) return;
      if (next === 'done') {
        celebrateTaskDone(intl.formatMessage({ id: 'tasks.celebrate.done' }));
        if (task.assigned_to) setBurst({ agentId: task.assigned_to });
      }
      moveTask(task.id, next);
    },
    [moveTask, intl],
  );

  const handleDrop = useCallback(
    (taskId: string, status: TaskStatus) => {
      const task = tasks.find((t) => t.id === taskId);
      if (task) applyStatus(task, status);
    },
    [tasks, applyStatus],
  );

  const handleListStatus = useCallback(
    (task: TaskInfo, key: import('@/components/ui').TaskStatusKey) => {
      const backend = toBackendStatus(key);
      if (backend) applyStatus(task, backend);
    },
    [applyStatus],
  );

  const handleRemoveConfirm = useCallback(async () => {
    if (removeTarget) {
      await removeTask(removeTarget.id);
      setRemoveTarget(null);
    }
  }, [removeTarget, removeTask]);

  const filteredTasks = useMemo(
    () =>
      tasks.filter((t) => {
        if (filterAgent && t.assigned_to !== filterAgent) return false;
        if (filterPriority && t.priority !== filterPriority) return false;
        return true;
      }),
    [tasks, filterAgent, filterPriority],
  );

  // Per-AI-staff buckets (incl. an unassigned bucket) — the single grouping
  // source shared by the list "by staff" view and the kanban swimlanes.
  const agentBuckets = useMemo(() => {
    const buckets = new Map<string, TaskInfo[]>();
    for (const t of filteredTasks) {
      const key = t.assigned_to || '__unassigned';
      (buckets.get(key) ?? buckets.set(key, []).get(key)!).push(t);
    }
    return Array.from(buckets.entries()).map(([key, rows]) => ({
      key,
      agentId: key === '__unassigned' ? undefined : key,
      label:
        key === '__unassigned'
          ? intl.formatMessage({ id: 'tasks.assignee.none' })
          : agents.find((a) => a.name === key)?.display_name ?? key,
      rows,
    }));
  }, [filteredTasks, agents, intl]);

  // List grouping — by status (column order) or by assignee (shared buckets).
  const listGroups = useMemo(() => {
    if (group === 'assignee') return agentBuckets;
    return COLUMNS.map(({ status }) => ({
      key: status,
      agentId: undefined as string | undefined,
      label: intl.formatMessage({ id: `tasks.column.${status}` }),
      rows: filteredTasks.filter((t) => t.status === status),
    })).filter((g) => g.rows.length > 0);
  }, [group, agentBuckets, filteredTasks, intl]);

  // First time the user lands on an assignee grouping with a large roster,
  // seed the swimlanes collapsed (unless they already have a saved preference).
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current || group !== 'assignee' || agentBuckets.length === 0) return;
    seededRef.current = true;
    let hasStored = false;
    try {
      hasStored = localStorage.getItem(COLLAPSE_KEY) != null;
    } catch {
      /* ignore */
    }
    if (!hasStored && agentBuckets.length >= MANY_AGENTS) {
      const all = new Set(agentBuckets.map((b) => b.key));
      setCollapsed(all);
      writeCollapsed(all);
    }
  }, [group, agentBuckets]);

  const tasksByStatus = (status: TaskStatus) => filteredTasks.filter((t) => t.status === status);

  const toggleGroup = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      writeCollapsed(next);
      return next;
    });

  return (
    <Page wide>
      <PageHeader
        icon={KanbanSquare}
        title={intl.formatMessage({ id: 'nav.tasks' })}
        subtitle={intl.formatMessage({ id: 'tasks.title' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowCreate(true)}>
            {intl.formatMessage({ id: 'tasks.create' })}
          </Button>
        }
      />

      {/* Toolbar: filters grouped on the left, view toggle pinned to the far right
          (justify-between) so the 看板/清單 switch reads as a distinct control, not
          another filter. */}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <Filter className="h-4 w-4 text-stone-400" />
          <select
            className={cn(controlClass, 'w-auto min-w-[140px]')}
            value={filterAgent ?? ''}
            onChange={(e) => setFilterAgent(e.target.value || null)}
          >
            <option value="">
              {intl.formatMessage({ id: 'tasks.filter.all' })} — {intl.formatMessage({ id: 'tasks.filter.agent' })}
            </option>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.display_name}
              </option>
            ))}
          </select>
          <select
            className={cn(controlClass, 'w-auto min-w-[140px]')}
            value={filterPriority ?? ''}
            onChange={(e) => setFilterPriority((e.target.value as TaskPriority) || null)}
          >
            <option value="">
              {intl.formatMessage({ id: 'tasks.filter.all' })} — {intl.formatMessage({ id: 'tasks.field.priority' })}
            </option>
            {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
              <option key={p} value={p}>
                {intl.formatMessage({ id: `tasks.priority.${p}` })}
              </option>
            ))}
          </select>

          {/* Group-by — applies to both the list groups and the kanban swimlanes. */}
          <select
            className={cn(controlClass, 'w-auto min-w-[130px]')}
            value={group}
            onChange={(e) => setGroupPref(e.target.value as GroupMode)}
            aria-label={intl.formatMessage({ id: 'tasks.groupBy' })}
          >
            <option value="status">{intl.formatMessage({ id: 'tasks.groupBy.status' })}</option>
            <option value="assignee">{intl.formatMessage({ id: 'tasks.groupBy.assignee' })}</option>
          </select>
        </div>

        {/* Kanban ⇄ List view toggle — pinned right, separated from the filters. */}
        <div className="inline-flex rounded-control border border-stone-300/50 p-0.5 dark:border-white/10">
          {(
            [
              { id: 'kanban', icon: KanbanSquare },
              { id: 'list', icon: List },
            ] as const
          ).map(({ id, icon: Icon }) => (
            <button
              key={id}
              type="button"
              onClick={() => setViewPref(id)}
              aria-pressed={view === id}
              className={cn(
                'flex items-center gap-1.5 rounded-[calc(var(--radius-control)-2px)] px-2.5 py-1 text-xs font-medium transition-colors',
                view === id
                  ? 'bg-amber-500/15 text-amber-700 dark:text-amber-300'
                  : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200',
              )}
            >
              <Icon className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: `tasks.view.${id}` })}
            </button>
          ))}
        </div>
      </div>

      {tasks.length === 0 && !loading && (
        <Card padded={false}>
          <EmptyState icon={Clock} title={intl.formatMessage({ id: 'tasks.empty' })} />
        </Card>
      )}

      {view === 'kanban' ? (
        group === 'assignee' ? (
          // By-staff swimlanes: one collapsible board per AI employee.
          <div className="space-y-4">
            {agentBuckets.map((b) => {
              const isCollapsed = collapsed.has(b.key);
              return (
                <div key={b.key}>
                  <GroupHeader
                    label={
                      b.agentId ? (
                        <span className="flex items-center gap-2">
                          <CharacterAvatar agentId={b.agentId} name={b.label} size={22} animated={false} />
                          {b.label}
                        </span>
                      ) : (
                        b.label
                      )
                    }
                    count={b.rows.length}
                    collapsed={isCollapsed}
                    onToggle={() => toggleGroup(b.key)}
                  />
                  {!isCollapsed && (
                    <div className="mt-2 grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-4">
                      {COLUMNS.map(({ status, icon }) => (
                        <KanbanColumn
                          key={status}
                          status={status}
                          icon={icon}
                          tasks={b.rows.filter((t) => t.status === status)}
                          agents={agents}
                          onDrop={handleDrop}
                          onOpen={openTask}
                          onRemove={setRemoveTarget}
                        />
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-4">
            {COLUMNS.map(({ status, icon }) => (
              <KanbanColumn
                key={status}
                status={status}
                icon={icon}
                tasks={tasksByStatus(status)}
                agents={agents}
                onDrop={handleDrop}
                onOpen={openTask}
                onRemove={setRemoveTarget}
              />
            ))}
          </div>
        )
      ) : (
        <div className="space-y-4">
          {listGroups.map((g) => {
            const isCollapsed = collapsed.has(g.key);
            return (
              <div key={g.key}>
                <GroupHeader label={g.label} count={g.rows.length} collapsed={isCollapsed} onToggle={() => toggleGroup(g.key)} />
                {!isCollapsed && (
                  <Card padded={false}>
                    <ul className="divide-y divide-stone-200/60 dark:divide-white/5">
                      {g.rows.map((t) => (
                        <TaskListRow
                          key={t.id}
                          task={t}
                          agent={agents.find((a) => a.name === t.assigned_to)}
                          onOpen={openTask}
                          onStatus={handleListStatus}
                        />
                      ))}
                    </ul>
                  </Card>
                )}
              </div>
            );
          })}
        </div>
      )}

      <CreateTaskModal
        open={showCreate}
        onClose={closeCreate}
        agents={agents}
        onCreate={handleCreate}
        defaultAssignee={defaultAssignee}
      />

      <Dialog open={removeTarget !== null} title={intl.formatMessage({ id: 'tasks.remove' })} onClose={() => setRemoveTarget(null)}>
        <div className="space-y-4">
          <p className="text-sm text-stone-600 dark:text-stone-400">
            {removeTarget && intl.formatMessage({ id: 'tasks.remove.confirm' }, { title: removeTarget.title })}
          </p>
          <div className="flex justify-end gap-3">
            <Button variant="secondary" onClick={() => setRemoveTarget(null)}>
              {intl.formatMessage({ id: 'agents.delegate.close' })}
            </Button>
            <Button variant="danger" onClick={handleRemoveConfirm}>
              {intl.formatMessage({ id: 'tasks.remove' })}
            </Button>
          </div>
        </div>
      </Dialog>

      {burst && (
        <TaskDoneBurst
          agentId={burst.agentId}
          agentName={agents.find((a) => a.name === burst.agentId)?.display_name}
          onDone={() => setBurst(null)}
        />
      )}
    </Page>
  );
}
