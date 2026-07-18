import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import {
  PageHeader,
  Button,
  Badge,
  Empty,
  Segmented,
  Checkbox,
  ActorAvatar,
  Popover,
  PopoverTrigger,
  PopoverContent,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  type SegmentedOption,
} from '@/components/mds';
import {
  StatusIcon,
  PriorityIcon,
  type TaskStatusKey,
} from '@/components/ui';
import { CreateTaskModal, TaskDoneBurst, celebrateTaskDone } from '@/components/task';
import { toStatusKey, toBackendStatus } from '@/lib/task-status';
import { timeAgo } from '@/lib/format';
import type { TaskInfo, TaskStatus, TaskPriority, TaskCreateParams } from '@/lib/api';
import {
  Plus,
  Trash2,
  Filter as FilterIcon,
  SlidersHorizontal,
  KanbanSquare,
  List as ListIcon,
  Check,
  X,
  ChevronDown,
} from 'lucide-react';

// ── Preferences (localStorage, §5.4 view memory) ────────────
const VIEW_KEY = 'duduclaw:tasks:view';
const GROUP_KEY = 'duduclaw:tasks:group';
const ORDER_KEY = 'duduclaw:tasks:order';
// Persisted set of collapsed group/swimlane keys (shared by the list group-by
// and the kanban by-agent swimlanes, both keyed by assignee).
const COLLAPSE_KEY = 'duduclaw:tasks:collapsed';
// When grouping by AI staff, auto-collapse the swimlanes once there are this
// many buckets — so a large roster doesn't unfurl into a wall of boards.
const MANY_AGENTS = 4;
type ViewMode = 'kanban' | 'list';
type GroupMode = 'status' | 'assignee';
type OrderMode = 'recent' | 'priority' | 'title';

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

const COLUMNS: ReadonlyArray<{ status: TaskStatus }> = [
  { status: 'todo' },
  { status: 'in_progress' },
  // P2a: autonomous goal tasks parked for a human decision (retry/done/abort).
  { status: 'needs_human' },
  { status: 'done' },
  { status: 'blocked' },
];

const PRIORITY_RANK: Record<TaskPriority, number> = { urgent: 0, high: 1, medium: 2, low: 3 };

/** Order rows within a group by the active ordering preference (pure). */
function orderRows(rows: ReadonlyArray<TaskInfo>, order: OrderMode): TaskInfo[] {
  const copy = [...rows];
  if (order === 'priority') {
    copy.sort((a, b) => PRIORITY_RANK[a.priority] - PRIORITY_RANK[b.priority] || b.updated_at.localeCompare(a.updated_at));
  } else if (order === 'title') {
    copy.sort((a, b) => a.title.localeCompare(b.title));
  } else {
    copy.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  }
  return copy;
}

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
      onClick={() => onOpen(task.id)}
      className="group/card w-64 shrink-0 cursor-grab rounded-lg border border-surface-border bg-surface p-3 shadow-[var(--surface-shadow)] transition-colors hover:bg-surface-hover active:cursor-grabbing"
    >
      <div className="flex items-start justify-between gap-2">
        <p className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{task.title}</p>
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onRemove(task);
          }}
          className="shrink-0 rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:text-destructive group-hover/card:opacity-100 pointer-coarse:opacity-100"
          title={intl.formatMessage({ id: 'tasks.remove' })}
          aria-label={intl.formatMessage({ id: 'tasks.remove' })}
        >
          <Trash2 className="size-3.5" />
        </button>
      </div>

      {task.description && (
        <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">{task.description}</p>
      )}

      <div className="mt-3 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <PriorityIcon priority={task.priority} size="sm" />
          <span className="font-mono text-xs tabular-nums text-muted-foreground">{timeAgo(task.updated_at)}</span>
        </div>
        {agent && (
          <ActorAvatar actorType="agent" size="sm" name={agent.display_name} aria-label={agent.display_name} />
        )}
      </div>

      {task.status === 'blocked' && task.blocked_reason && (
        <div className="mt-2 rounded-md bg-destructive/10 px-2 py-1 text-xs text-destructive">{task.blocked_reason}</div>
      )}
      {task.status === 'needs_human' && (
        <div className="mt-2 rounded-md bg-destructive/10 px-2 py-1 text-xs text-destructive">
          <span className="font-medium">{intl.formatMessage({ id: 'tasks.column.needs_human' })}</span>
          {task.judge_feedback ? `：${task.judge_feedback}` : ''}
        </div>
      )}
    </div>
  );
}

// ── Kanban column ───────────────────────────────────────────
function KanbanColumn({
  status,
  tasks,
  agents,
  onDrop,
  onOpen,
  onRemove,
  onAdd,
}: {
  status: TaskStatus;
  tasks: ReadonlyArray<TaskInfo>;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  onDrop: (taskId: string, status: TaskStatus) => void;
  onOpen: (id: string) => void;
  onRemove: (task: TaskInfo) => void;
  onAdd: () => void;
}) {
  const intl = useIntl();
  const [isDragOver, setIsDragOver] = useState(false);

  return (
    <div className="flex w-70 shrink-0 flex-col">
      <div className="flex items-center gap-2 px-1 pb-2">
        <StatusIcon status={toStatusKey(status)} size="sm" />
        <h3 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: `tasks.column.${status}` })}
        </h3>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{tasks.length}</span>
        <button
          type="button"
          onClick={onAdd}
          className="ml-auto rounded p-0.5 text-muted-foreground hover:bg-surface-hover hover:text-foreground"
          title={intl.formatMessage({ id: 'tasks.create' })}
          aria-label={`${intl.formatMessage({ id: 'tasks.create' })} · ${intl.formatMessage({ id: `tasks.column.${status}` })}`}
        >
          <Plus className="size-4" />
        </button>
      </div>
      <div
        className={cn(
          'flex min-h-[200px] flex-col gap-2 rounded-xl bg-muted/40 p-2 transition-shadow',
          isDragOver && 'ring-2 ring-brand/25',
        )}
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
          <p className="px-2 py-6 text-center text-xs text-muted-foreground/70">
            {intl.formatMessage({ id: 'tasks.dropHint' })}
          </p>
        )}
      </div>
    </div>
  );
}

/** One horizontal board (five status columns), shared by the flat + swimlane views. */
function KanbanBoard({
  rows,
  agents,
  onDrop,
  onOpen,
  onRemove,
  onAdd,
}: {
  rows: ReadonlyArray<TaskInfo>;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  onDrop: (taskId: string, status: TaskStatus) => void;
  onOpen: (id: string) => void;
  onRemove: (task: TaskInfo) => void;
  onAdd: () => void;
}) {
  return (
    <div className="flex gap-4 overflow-x-auto p-2">
      {COLUMNS.map(({ status }) => (
        <KanbanColumn
          key={status}
          status={status}
          tasks={rows.filter((t) => t.status === status)}
          agents={agents}
          onDrop={onDrop}
          onOpen={onOpen}
          onRemove={onRemove}
          onAdd={onAdd}
        />
      ))}
    </div>
  );
}

// ── List row ────────────────────────────────────────────────
function TaskListRow({
  task,
  agent,
  selected,
  onToggleSelect,
  onOpen,
  onStatus,
}: {
  task: TaskInfo;
  agent?: { name: string; display_name: string };
  selected: boolean;
  onToggleSelect: (id: string) => void;
  onOpen: (id: string) => void;
  onStatus: (task: TaskInfo, key: TaskStatusKey) => void;
}) {
  const intl = useIntl();
  return (
    <li
      onClick={() => onOpen(task.id)}
      className={cn(
        'group/row flex h-9 cursor-pointer items-center gap-2 px-4 text-sm transition-colors',
        selected ? 'bg-surface-selected' : 'hover:bg-surface-hover',
      )}
    >
      {/* Selection checkbox — reveals on hover, sticky once selected. */}
      <span
        onClick={(e) => e.stopPropagation()}
        className={cn(
          'flex shrink-0 items-center transition-opacity',
          selected ? 'opacity-100' : 'opacity-0 group-hover/row:opacity-100 pointer-coarse:opacity-100',
        )}
      >
        <Checkbox
          checked={selected}
          onCheckedChange={() => onToggleSelect(task.id)}
          aria-label={intl.formatMessage({ id: 'tasks.select.toggle' })}
        />
      </span>

      {/* Inline status editor (kept: list quick-status change). */}
      <span onClick={(e) => e.stopPropagation()} className="flex shrink-0 items-center">
        <StatusIcon status={toStatusKey(task.status)} size="sm" onChange={(key) => onStatus(task, key)} />
      </span>

      <span className="hidden w-16 shrink-0 font-mono text-xs tabular-nums text-muted-foreground md:inline">
        {task.id.slice(0, 8)}
      </span>

      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onOpen(task.id);
        }}
        className="min-w-0 flex-1 truncate text-left text-sm text-foreground hover:text-foreground/80"
      >
        {task.title}
      </button>

      {task.tags.length > 0 && (
        <span className="hidden shrink-0 items-center gap-1 md:flex">
          {task.tags.slice(0, 3).map((tag) => (
            <Badge key={tag} variant="secondary" className="max-w-24 truncate">
              {tag}
            </Badge>
          ))}
        </span>
      )}

      <PriorityIcon priority={task.priority} size="sm" className="shrink-0" />

      <span className="hidden w-10 shrink-0 text-right font-mono text-xs tabular-nums text-muted-foreground sm:inline">
        {timeAgo(task.updated_at)}
      </span>

      {agent && (
        <span className="hidden shrink-0 sm:inline-flex" title={agent.display_name}>
          <ActorAvatar actorType="agent" size="sm" name={agent.display_name} aria-label={agent.display_name} />
        </span>
      )}
    </li>
  );
}

/** Plain Multica swimlane header (status/agent glyph + name + count + collapse). */
function SwimlaneHeader({
  label,
  avatar,
  count,
  collapsed,
  onToggle,
}: {
  label: string;
  avatar?: React.ReactNode;
  count: number;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={!collapsed}
      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-surface-hover"
    >
      <ChevronDown className={cn('size-4 shrink-0 text-muted-foreground transition-transform', collapsed && '-rotate-90')} />
      {avatar}
      <span className="truncate text-sm font-medium text-foreground">{label}</span>
      <span className="font-mono text-xs tabular-nums text-muted-foreground">{count}</span>
    </button>
  );
}

// ── Filter menu (nested-style grouped DropdownMenu) ──────────
function FilterMenu({
  agents,
  filterAgent,
  filterPriority,
  setFilterAgent,
  setFilterPriority,
}: {
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  filterAgent: string | null;
  filterPriority: TaskPriority | null;
  setFilterAgent: (v: string | null) => void;
  setFilterPriority: (v: TaskPriority | null) => void;
}) {
  const intl = useIntl();
  const activeCount = (filterAgent ? 1 : 0) + (filterPriority ? 1 : 0);
  const active = activeCount > 0;

  const clearAll = () => {
    setFilterAgent(null);
    setFilterPriority(null);
  };

  return (
    <div className="flex shrink-0 items-center">
      <DropdownMenu>
        <DropdownMenuTrigger
          className={cn(
            'inline-flex h-7 items-center gap-1 rounded-lg border px-2.5 text-[0.8rem] font-medium outline-none transition-colors focus-visible:ring-3 focus-visible:ring-ring/50',
            active
              ? 'border-brand bg-brand text-brand-foreground hover:bg-brand/90'
              : 'border-border bg-background text-foreground hover:bg-muted',
          )}
        >
          <FilterIcon className="size-3.5" />
          {intl.formatMessage({ id: 'tasks.filter.title' })}
          {active && <span className="font-mono text-xs tabular-nums">{activeCount}</span>}
        </DropdownMenuTrigger>
        <DropdownMenuContent className="min-w-52">
          <DropdownMenuLabel>{intl.formatMessage({ id: 'tasks.field.priority' })}</DropdownMenuLabel>
          <DropdownMenuItem
            closeOnClick={false}
            onClick={() => setFilterPriority(null)}
            className={cn(!filterPriority && 'font-medium text-foreground')}
          >
            <span className="flex-1">{intl.formatMessage({ id: 'tasks.filter.all' })}</span>
            {!filterPriority && <Check className="size-3.5 text-brand" />}
          </DropdownMenuItem>
          {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
            <DropdownMenuItem
              key={p}
              closeOnClick={false}
              onClick={() => setFilterPriority(p)}
              className={cn(filterPriority === p && 'font-medium text-foreground')}
            >
              <PriorityIcon priority={p} size="sm" />
              <span className="flex-1">{intl.formatMessage({ id: `tasks.priority.${p}` })}</span>
              {filterPriority === p && <Check className="size-3.5 text-brand" />}
            </DropdownMenuItem>
          ))}

          <DropdownMenuSeparator />
          <DropdownMenuLabel>{intl.formatMessage({ id: 'tasks.field.assignTo' })}</DropdownMenuLabel>
          <DropdownMenuItem
            closeOnClick={false}
            onClick={() => setFilterAgent(null)}
            className={cn(!filterAgent && 'font-medium text-foreground')}
          >
            <span className="flex-1">{intl.formatMessage({ id: 'tasks.filter.all' })}</span>
            {!filterAgent && <Check className="size-3.5 text-brand" />}
          </DropdownMenuItem>
          {agents.map((a) => (
            <DropdownMenuItem
              key={a.name}
              closeOnClick={false}
              onClick={() => setFilterAgent(a.name)}
              className={cn(filterAgent === a.name && 'font-medium text-foreground')}
            >
              <ActorAvatar actorType="agent" size="xs" name={a.display_name} />
              <span className="min-w-0 flex-1 truncate">{a.display_name}</span>
              {filterAgent === a.name && <Check className="size-3.5 shrink-0 text-brand" />}
            </DropdownMenuItem>
          ))}

          {active && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={clearAll}>
                <X className="size-3.5" />
                {intl.formatMessage({ id: 'tasks.filter.clear' })}
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

// ── Display popover (grouping + ordering) ────────────────────
function DisplayPopover({
  group,
  setGroup,
  order,
  setOrder,
}: {
  group: GroupMode;
  setGroup: (g: GroupMode) => void;
  order: OrderMode;
  setOrder: (o: OrderMode) => void;
}) {
  const intl = useIntl();
  const groupOptions: SegmentedOption<GroupMode>[] = [
    { value: 'status', label: intl.formatMessage({ id: 'tasks.groupBy.status' }) },
    { value: 'assignee', label: intl.formatMessage({ id: 'tasks.groupBy.assignee' }) },
  ];
  const orderOptions: SegmentedOption<OrderMode>[] = [
    { value: 'recent', label: intl.formatMessage({ id: 'tasks.order.recent' }) },
    { value: 'priority', label: intl.formatMessage({ id: 'tasks.order.priority' }) },
    { value: 'title', label: intl.formatMessage({ id: 'tasks.order.title' }) },
  ];
  return (
    <Popover>
      <PopoverTrigger
        className="inline-flex h-7 shrink-0 items-center gap-1 rounded-lg border border-border bg-background px-2.5 text-[0.8rem] font-medium text-foreground outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <SlidersHorizontal className="size-3.5" />
        {intl.formatMessage({ id: 'tasks.display' })}
      </PopoverTrigger>
      <PopoverContent align="end" className="w-64 space-y-3 p-3">
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">
            {intl.formatMessage({ id: 'tasks.display.grouping' })}
          </p>
          <Segmented
            value={group}
            onValueChange={setGroup}
            options={groupOptions}
            aria-label={intl.formatMessage({ id: 'tasks.display.grouping' })}
          />
        </div>
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-muted-foreground">
            {intl.formatMessage({ id: 'tasks.display.ordering' })}
          </p>
          <Segmented
            value={order}
            onValueChange={setOrder}
            options={orderOptions}
            aria-label={intl.formatMessage({ id: 'tasks.display.ordering' })}
          />
        </div>
      </PopoverContent>
    </Popover>
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
  const [order, setOrder] = useState<OrderMode>(() => readPref(ORDER_KEY, ['recent', 'priority', 'title'] as const, 'recent'));
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(readCollapsed);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [showCreate, setShowCreate] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<TaskInfo | null>(null);
  const [confirmBatch, setConfirmBatch] = useState(false);
  const [burst, setBurst] = useState<{ agentId: string } | null>(null);

  useEffect(() => {
    fetchTasks();
    fetchAgents();
  }, [fetchTasks, fetchAgents]);

  // Open the create modal when routed here with `?new=1` (Sidebar / MobileBottomNav).
  useEffect(() => {
    if (searchParams.get('new') === '1') setShowCreate(true);
  }, [searchParams]);

  // `?assignee=<id>` (employee-detail "交辦" button) preselects the assignee.
  const defaultAssignee = searchParams.get('assignee') || undefined;

  const setViewPref = (v: ViewMode) => {
    setView(v);
    writePref(VIEW_KEY, v);
  };
  const setGroupPref = (g: GroupMode) => {
    setGroup(g);
    writePref(GROUP_KEY, g);
  };
  const setOrderPref = (o: OrderMode) => {
    setOrder(o);
    writePref(ORDER_KEY, o);
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

  // One completion path for drag-drop, list StatusIcon, and batch: write via the
  // store, and fire the §5.5 celebration when a task first reaches `done`.
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
    (task: TaskInfo, key: TaskStatusKey) => {
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

  // ── Selection (batch) ────────────────────────────────────
  const toggleSelect = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const clearSelection = useCallback(() => setSelected(new Set()), []);

  const filteredTasks = useMemo(
    () =>
      tasks.filter((t) => {
        if (filterAgent && t.assigned_to !== filterAgent) return false;
        if (filterPriority && t.priority !== filterPriority) return false;
        return true;
      }),
    [tasks, filterAgent, filterPriority],
  );

  // Prune the selection to what's still visible (filters can hide selected rows).
  useEffect(() => {
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const visible = new Set(filteredTasks.map((t) => t.id));
      const next = new Set([...prev].filter((id) => visible.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [filteredTasks]);

  const selectedTasks = useMemo(
    () => filteredTasks.filter((t) => selected.has(t.id)),
    [filteredTasks, selected],
  );

  const handleBatchDone = useCallback(() => {
    for (const t of selectedTasks) applyStatus(t, 'done');
    clearSelection();
  }, [selectedTasks, applyStatus, clearSelection]);

  const handleBatchDelete = useCallback(async () => {
    for (const t of selectedTasks) await removeTask(t.id);
    clearSelection();
    setConfirmBatch(false);
  }, [selectedTasks, removeTask, clearSelection]);

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
    if (group === 'assignee') {
      return agentBuckets.map((b) => ({ ...b, rows: orderRows(b.rows, order) }));
    }
    return COLUMNS.map(({ status }) => ({
      key: status,
      agentId: undefined as string | undefined,
      label: intl.formatMessage({ id: `tasks.column.${status}` }),
      rows: orderRows(filteredTasks.filter((t) => t.status === status), order),
    })).filter((g) => g.rows.length > 0);
  }, [group, agentBuckets, filteredTasks, intl, order]);

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

  const toggleGroup = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      writeCollapsed(next);
      return next;
    });

  const openCreate = () => setShowCreate(true);

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      {/* Layer 1 — page header (icon + title + count). */}
      <PageHeader hideTrigger className="px-5">
        <KanbanSquare className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.tasks' })}</h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{filteredTasks.length}</span>
        <div className="ml-auto flex items-center gap-2">
          <Button variant="brand" size="sm" onClick={openCreate}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'tasks.create' })}</span>
          </Button>
        </div>
      </PageHeader>

      {/* Layer 2 — control row (scope · filter / display / view). */}
      <div className="flex h-12 shrink-0 items-center gap-2 overflow-x-auto border-b border-surface-border px-4">
        <span className="mr-auto shrink-0 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'tasks.scope.all' })}
        </span>
        <FilterMenu
          agents={agents}
          filterAgent={filterAgent}
          filterPriority={filterPriority}
          setFilterAgent={setFilterAgent}
          setFilterPriority={setFilterPriority}
        />
        <DisplayPopover group={group} setGroup={setGroupPref} order={order} setOrder={setOrderPref} />
        <Segmented
          value={view}
          onValueChange={setViewPref}
          aria-label={intl.formatMessage({ id: 'tasks.groupBy' })}
          options={[
            {
              value: 'kanban',
              label: (
                <>
                  <KanbanSquare className="size-3.5" />
                  <span className="sr-only">{intl.formatMessage({ id: 'tasks.view.kanban' })}</span>
                </>
              ),
            },
            {
              value: 'list',
              label: (
                <>
                  <ListIcon className="size-3.5" />
                  <span className="sr-only">{intl.formatMessage({ id: 'tasks.view.list' })}</span>
                </>
              ),
            },
          ]}
        />
      </div>

      {/* Body */}
      <div className="min-h-[50vh] pb-16 pt-2">
        {filteredTasks.length === 0 && !loading ? (
          <Empty
            icon={KanbanSquare}
            title={intl.formatMessage({ id: 'tasks.empty' })}
            action={
              <Button variant="brand" size="sm" onClick={openCreate}>
                <Plus />
                {intl.formatMessage({ id: 'tasks.create' })}
              </Button>
            }
          />
        ) : view === 'kanban' ? (
          group === 'assignee' ? (
            <div className="space-y-2 px-2">
              {agentBuckets.map((b) => {
                const isCollapsed = collapsed.has(b.key);
                return (
                  <div key={b.key}>
                    <SwimlaneHeader
                      label={b.label}
                      avatar={
                        b.agentId ? (
                          <ActorAvatar actorType="agent" size="sm" name={b.label} />
                        ) : undefined
                      }
                      count={b.rows.length}
                      collapsed={isCollapsed}
                      onToggle={() => toggleGroup(b.key)}
                    />
                    {!isCollapsed && (
                      <KanbanBoard
                        rows={b.rows}
                        agents={agents}
                        onDrop={handleDrop}
                        onOpen={openTask}
                        onRemove={setRemoveTarget}
                        onAdd={openCreate}
                      />
                    )}
                  </div>
                );
              })}
            </div>
          ) : (
            <KanbanBoard
              rows={filteredTasks}
              agents={agents}
              onDrop={handleDrop}
              onOpen={openTask}
              onRemove={setRemoveTarget}
              onAdd={openCreate}
            />
          )
        ) : (
          <div className="space-y-3 px-2">
            {listGroups.map((g) => {
              const isCollapsed = collapsed.has(g.key);
              return (
                <div key={g.key}>
                  <SwimlaneHeader
                    label={g.label}
                    avatar={
                      group === 'assignee'
                        ? g.agentId
                          ? <ActorAvatar actorType="agent" size="sm" name={g.label} />
                          : undefined
                        : <StatusIcon status={toStatusKey(g.key as TaskStatus)} size="sm" />
                    }
                    count={g.rows.length}
                    collapsed={isCollapsed}
                    onToggle={() => toggleGroup(g.key)}
                  />
                  {!isCollapsed && (
                    <ul className="mt-1 overflow-hidden rounded-xl border border-surface-border bg-surface">
                      {g.rows.map((t) => (
                        <TaskListRow
                          key={t.id}
                          task={t}
                          agent={agents.find((a) => a.name === t.assigned_to)}
                          selected={selected.has(t.id)}
                          onToggleSelect={toggleSelect}
                          onOpen={openTask}
                          onStatus={handleListStatus}
                        />
                      ))}
                    </ul>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Batch action toolbar — floats bottom-center while rows are selected. */}
      {selected.size > 0 && (
        <div className="pointer-events-none fixed inset-x-0 bottom-6 z-40 flex justify-center px-4">
          <div className="pointer-events-auto flex items-center gap-2 rounded-xl bg-surface-raised px-3 py-2 shadow-[var(--floating-shadow)] ring-1 ring-surface-border">
            <span className="px-1 text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'tasks.select.count' }, { count: selected.size })}
            </span>
            <Button variant="outline" size="sm" onClick={handleBatchDone}>
              <Check />
              {intl.formatMessage({ id: 'tasks.batch.markDone' })}
            </Button>
            <Button variant="destructive" size="sm" onClick={() => setConfirmBatch(true)}>
              <Trash2 />
              {intl.formatMessage({ id: 'tasks.batch.delete' })}
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={clearSelection}
              aria-label={intl.formatMessage({ id: 'tasks.select.clear' })}
            >
              <X />
            </Button>
          </div>
        </div>
      )}

      <CreateTaskModal
        open={showCreate}
        onClose={closeCreate}
        agents={agents}
        onCreate={handleCreate}
        defaultAssignee={defaultAssignee}
      />

      {/* Single-task delete confirmation. */}
      <Dialog open={removeTarget !== null} onOpenChange={(o) => !o && setRemoveTarget(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'tasks.remove' })}</DialogTitle>
            <DialogDescription>
              {removeTarget && intl.formatMessage({ id: 'tasks.remove.confirm' }, { title: removeTarget.title })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'agents.delegate.close' })}</Button>}
            />
            <Button variant="destructive" onClick={handleRemoveConfirm}>
              {intl.formatMessage({ id: 'tasks.remove' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Batch delete confirmation. */}
      <Dialog open={confirmBatch} onOpenChange={setConfirmBatch}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'tasks.batch.delete' })}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage({ id: 'tasks.batch.deleteConfirm' }, { count: selected.size })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'agents.delegate.close' })}</Button>}
            />
            <Button variant="destructive" onClick={handleBatchDelete}>
              {intl.formatMessage({ id: 'tasks.batch.delete' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {burst && (
        <TaskDoneBurst
          agentId={burst.agentId}
          agentName={agents.find((a) => a.name === burst.agentId)?.display_name}
          onDone={() => setBurst(null)}
        />
      )}
    </div>
  );
}
