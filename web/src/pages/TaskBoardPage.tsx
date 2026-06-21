import { useEffect, useState, useCallback, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import { Dialog, FormField, inputClass, selectClass } from '@/components/shared/Dialog';
import { Page, PageHeader, Card, Badge, Button, EmptyState, Toolbar, controlClass } from '@/components/ui';
import type { TaskInfo, TaskStatus, TaskPriority, TaskCreateParams } from '@/lib/api';
import type { TaskUpdateParams } from '@/lib/api';
import {
  Plus,
  GripVertical,
  AlertCircle,
  Clock,
  CheckCircle2,
  Ban,
  Flag,
  Filter,
  Trash2,
  X,
  Pencil,
  Save,
  User,
  Calendar,
  Link2,
  KanbanSquare,
} from 'lucide-react';

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

// ── Priority Badge ──────────────────────────────────────────

const PRIORITY_TONES: Record<TaskPriority, 'neutral' | 'info' | 'warning' | 'danger'> = {
  low: 'neutral',
  medium: 'info',
  high: 'warning',
  urgent: 'danger',
};

function PriorityBadge({ priority }: { priority: TaskPriority }) {
  const intl = useIntl();
  return (
    <Badge tone={PRIORITY_TONES[priority]}>
      <Flag className="h-3 w-3" />
      {intl.formatMessage({ id: `tasks.priority.${priority}` })}
    </Badge>
  );
}

// ── Task Card ───────────────────────────────────────────────

function TaskCard({
  task,
  agents,
  onRemove,
  onSelect,
}: {
  task: TaskInfo;
  agents: ReadonlyArray<{ name: string; display_name: string; icon: string }>;
  onRemove: (id: string) => void;
  onSelect: (task: TaskInfo) => void;
}) {
  const intl = useIntl();
  const agent = agents.find((a) => a.name === task.assigned_to);

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
      className="panel panel-hover group cursor-grab rounded-lg p-3 active:cursor-grabbing"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="flex items-start gap-2">
          <GripVertical className="mt-0.5 h-4 w-4 flex-shrink-0 text-stone-300 dark:text-stone-600" />
          <div className="min-w-0">
            <button
              onClick={(e) => { e.stopPropagation(); onSelect(task); }}
              className="text-left text-sm font-medium text-stone-900 hover:text-amber-600 dark:text-stone-50 dark:hover:text-amber-400"
            >
              {task.title}
            </button>
            {task.description && (
              <p className="mt-1 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">
                {task.description}
              </p>
            )}
          </div>
        </div>
        <button
          onClick={() => onRemove(task.id)}
          className="flex-shrink-0 opacity-0 transition-opacity group-hover:opacity-100"
          title={intl.formatMessage({ id: 'tasks.remove' })}
        >
          <Trash2 className="h-3.5 w-3.5 text-stone-400 hover:text-rose-500" />
        </button>
      </div>

      <div className="mt-3 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <PriorityBadge priority={task.priority} />
          {task.tags.length > 0 && (
            <Badge tone="neutral">
              {task.tags[0]}
              {task.tags.length > 1 && ` +${task.tags.length - 1}`}
            </Badge>
          )}
        </div>
        {agent && (
          <div className="flex items-center gap-1.5" title={agent.display_name}>
            <span className="text-sm">{agent.icon || '🤖'}</span>
            <span className="max-w-[80px] truncate text-xs text-stone-500 dark:text-stone-400">
              {agent.display_name}
            </span>
          </div>
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

// ── Kanban Column ───────────────────────────────────────────

function KanbanColumn({
  status,
  icon: Icon,
  tasks,
  agents,
  onDrop,
  onRemove,
  onSelect,
}: {
  status: TaskStatus;
  icon: React.ComponentType<{ className?: string }>;
  tasks: ReadonlyArray<TaskInfo>;
  agents: ReadonlyArray<{ name: string; display_name: string; icon: string }>;
  onDrop: (taskId: string, status: TaskStatus) => void;
  onRemove: (id: string) => void;
  onSelect: (task: TaskInfo) => void;
}) {
  const intl = useIntl();
  const [isDragOver, setIsDragOver] = useState(false);

  const handleDragOver = useCallback((e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    setIsDragOver(true);
  }, []);

  const handleDragLeave = useCallback(() => {
    setIsDragOver(false);
  }, []);

  const handleDrop = useCallback(
    (e: DragEvent<HTMLDivElement>) => {
      e.preventDefault();
      setIsDragOver(false);
      const taskId = e.dataTransfer.getData('text/plain');
      if (taskId) {
        onDrop(taskId, status);
      }
    },
    [onDrop, status],
  );

  return (
    <div
      className={cn(
        'panel flex min-h-[300px] flex-col border-t-4',
        COLUMN_STYLES[status],
        isDragOver && 'ring-2 ring-amber-400/50',
      )}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-4 py-3">
        <div className="flex items-center gap-2">
          <Icon className="h-4 w-4 text-stone-500 dark:text-stone-400" />
          <h3 className="text-sm font-semibold text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: `tasks.column.${status}` })}
          </h3>
          <Badge tone="neutral" className="tabular-nums">{tasks.length}</Badge>
        </div>
      </div>

      <div className="flex-1 space-y-2 p-3">
        {tasks.map((task) => (
          <TaskCard key={task.id} task={task} agents={agents} onRemove={onRemove} onSelect={onSelect} />
        ))}
        {tasks.length === 0 && (
          <div
            className={cn(
              'flex items-center justify-center rounded-lg border-2 border-dashed py-8 text-xs text-stone-400 transition-colors dark:text-stone-600',
              isDragOver
                ? 'border-amber-400 bg-amber-50/50 dark:bg-amber-900/10'
                : 'border-[var(--panel-border)]',
            )}
          >
            {intl.formatMessage({ id: 'tasks.dropHint' })}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Create Task Dialog ──────────────────────────────────────

function CreateTaskDialog({
  open,
  onClose,
  agents,
  onCreate,
}: {
  open: boolean;
  onClose: () => void;
  agents: ReadonlyArray<{ name: string; display_name: string; icon: string }>;
  onCreate: (params: TaskCreateParams) => Promise<void>;
}) {
  const intl = useIntl();
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [assignedTo, setAssignedTo] = useState(agents[0]?.name ?? '');
  const [priority, setPriority] = useState<TaskPriority>('medium');
  const [tagsInput, setTagsInput] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = useCallback(async () => {
    if (!title.trim() || !assignedTo) return;
    setSubmitting(true);
    try {
      await onCreate({
        title: title.trim(),
        description: description.trim() || undefined,
        assigned_to: assignedTo,
        priority,
        tags: tagsInput
          .split(',')
          .map((t) => t.trim())
          .filter(Boolean),
      });
      setTitle('');
      setDescription('');
      setTagsInput('');
      onClose();
    } finally {
      setSubmitting(false);
    }
  }, [title, description, assignedTo, priority, tagsInput, onCreate, onClose]);

  return (
    <Dialog
      open={open}
      title={intl.formatMessage({ id: 'tasks.create' })}
      onClose={onClose}
    >
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'tasks.field.title' })}>
          <input
            className={inputClass}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={intl.formatMessage({ id: 'tasks.field.title' })}
            autoFocus
          />
        </FormField>

        <FormField label={intl.formatMessage({ id: 'tasks.field.description' })}>
          <textarea
            className={cn(inputClass, 'min-h-[80px] resize-y')}
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder={intl.formatMessage({ id: 'tasks.field.description' })}
          />
        </FormField>

        <FormField label={intl.formatMessage({ id: 'tasks.field.assignTo' })}>
          <select className={selectClass} value={assignedTo} onChange={(e) => setAssignedTo(e.target.value)}>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.icon || '🤖'} {a.display_name}
              </option>
            ))}
          </select>
        </FormField>

        <FormField label={intl.formatMessage({ id: 'tasks.field.priority' })}>
          <select
            className={selectClass}
            value={priority}
            onChange={(e) => setPriority(e.target.value as TaskPriority)}
          >
            {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
              <option key={p} value={p}>
                {intl.formatMessage({ id: `tasks.priority.${p}` })}
              </option>
            ))}
          </select>
        </FormField>

        <FormField label={intl.formatMessage({ id: 'tasks.field.tags' })}>
          <input
            className={inputClass}
            value={tagsInput}
            onChange={(e) => setTagsInput(e.target.value)}
            placeholder="bug, feature, docs"
          />
        </FormField>

        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: 'agents.delegate.close' })}
          </Button>
          <Button
            variant="primary"
            onClick={handleSubmit}
            disabled={submitting || !title.trim()}
          >
            {submitting
              ? intl.formatMessage({ id: 'agents.delegate.submitting' })
              : intl.formatMessage({ id: 'tasks.create' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Task Detail Panel (side-sliding) ────────────────────────

function TaskDetailPanel({
  task,
  agents,
  onClose,
  onUpdate,
}: {
  task: TaskInfo;
  agents: ReadonlyArray<{ name: string; display_name: string; icon: string }>;
  onClose: () => void;
  onUpdate: (taskId: string, fields: TaskUpdateParams) => Promise<void>;
}) {
  const intl = useIntl();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(task.title);
  const [description, setDescription] = useState(task.description);
  const [priority, setPriority] = useState<TaskPriority>(task.priority);
  const [assignedTo, setAssignedTo] = useState(task.assigned_to);
  const [blockedReason, setBlockedReason] = useState(task.blocked_reason ?? '');
  const [tagsInput, setTagsInput] = useState(task.tags.join(', '));
  const [saving, setSaving] = useState(false);

  const agent = agents.find((a) => a.name === task.assigned_to);

  const handleSave = useCallback(async () => {
    setSaving(true);
    try {
      await onUpdate(task.id, {
        title: title.trim(),
        description: description.trim(),
        priority,
        assigned_to: assignedTo,
        blocked_reason: blockedReason.trim() || undefined,
        tags: tagsInput.split(',').map((t) => t.trim()).filter(Boolean),
      });
      setEditing(false);
    } finally {
      setSaving(false);
    }
  }, [task.id, title, description, priority, assignedTo, blockedReason, tagsInput, onUpdate]);

  const formatDate = (d?: string) =>
    d ? new Date(d).toLocaleString('zh-TW', { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '—';

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 z-40 bg-black/20 backdrop-blur-sm" onClick={onClose} />

      {/* Panel */}
      <div className="glass-overlay fixed inset-y-0 right-0 z-50 flex w-full max-w-md flex-col border-l border-[var(--panel-border)] animate-in slide-in-from-right duration-200">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-6 py-4">
          <h3 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'tasks.detail' })}
          </h3>
          <div className="flex items-center gap-2">
            {!editing ? (
              <Button variant="ghost" size="sm" icon={Pencil} onClick={() => setEditing(true)} />
            ) : (
              <Button variant="ghost" size="sm" icon={Save} onClick={handleSave} disabled={saving} />
            )}
            <Button variant="ghost" size="sm" icon={X} onClick={onClose} />
          </div>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto px-6 py-5">
          <div className="space-y-5">
            {/* Title */}
            {editing ? (
              <input
                className={cn(inputClass, 'text-lg font-semibold')}
                value={title}
                onChange={(e) => setTitle(e.target.value)}
              />
            ) : (
              <h4 className="text-lg font-semibold text-stone-900 dark:text-stone-50">{task.title}</h4>
            )}

            {/* Status + Priority row */}
            <div className="flex items-center gap-3">
              <Badge
                tone={
                  task.status === 'done'
                    ? 'success'
                    : task.status === 'in_progress'
                      ? 'warning'
                      : task.status === 'blocked'
                        ? 'danger'
                        : 'neutral'
                }
              >
                {intl.formatMessage({ id: `tasks.status.${task.status}` })}
              </Badge>
              {editing ? (
                <select className={cn(selectClass, 'w-auto')} value={priority} onChange={(e) => setPriority(e.target.value as TaskPriority)}>
                  {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
                    <option key={p} value={p}>{intl.formatMessage({ id: `tasks.priority.${p}` })}</option>
                  ))}
                </select>
              ) : (
                <PriorityBadge priority={task.priority} />
              )}
            </div>

            {/* Description */}
            <div>
              <label className="mb-1.5 block text-xs font-medium text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'tasks.field.description' })}
              </label>
              {editing ? (
                <textarea
                  className={cn(inputClass, 'min-h-[100px] resize-y')}
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                />
              ) : (
                <p className="whitespace-pre-wrap text-sm text-stone-700 dark:text-stone-300">
                  {task.description || '—'}
                </p>
              )}
            </div>

            {/* Assigned to */}
            <div>
              <label className="mb-1.5 block text-xs font-medium text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'tasks.field.assignTo' })}
              </label>
              {editing ? (
                <select className={selectClass} value={assignedTo} onChange={(e) => setAssignedTo(e.target.value)}>
                  {agents.map((a) => (
                    <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
                  ))}
                </select>
              ) : (
                <div className="flex items-center gap-2">
                  <span className="text-lg">{agent?.icon || '🤖'}</span>
                  <span className="text-sm text-stone-700 dark:text-stone-300">{agent?.display_name ?? task.assigned_to}</span>
                </div>
              )}
            </div>

            {/* Blocked reason (only for blocked tasks) */}
            {(task.status === 'blocked' || editing) && (
              <div>
                <label className="mb-1.5 block text-xs font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'tasks.field.blockedReason' })}
                </label>
                {editing ? (
                  <input className={inputClass} value={blockedReason} onChange={(e) => setBlockedReason(e.target.value)} />
                ) : (
                  <p className="text-sm text-rose-600 dark:text-rose-400">{task.blocked_reason || '—'}</p>
                )}
              </div>
            )}

            {/* Tags */}
            <div>
              <label className="mb-1.5 block text-xs font-medium text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'tasks.field.tags' })}
              </label>
              {editing ? (
                <input className={inputClass} value={tagsInput} onChange={(e) => setTagsInput(e.target.value)} placeholder="bug, feature" />
              ) : (
                <div className="flex flex-wrap gap-1.5">
                  {task.tags.length > 0 ? task.tags.map((tag) => (
                    <Badge key={tag} tone="neutral">{tag}</Badge>
                  )) : <span className="text-sm text-stone-400">—</span>}
                </div>
              )}
            </div>

            {/* Metadata section */}
            <div className="space-y-3 border-t border-[var(--panel-border)] pt-4">
              <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                <User className="h-3.5 w-3.5" />
                <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.createdBy' })}</span>
                <span className="ml-auto text-stone-700 dark:text-stone-300">{task.created_by}</span>
              </div>
              <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                <Calendar className="h-3.5 w-3.5" />
                <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.createdAt' })}</span>
                <span className="ml-auto text-stone-700 dark:text-stone-300">{formatDate(task.created_at)}</span>
              </div>
              <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                <Calendar className="h-3.5 w-3.5" />
                <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.updatedAt' })}</span>
                <span className="ml-auto text-stone-700 dark:text-stone-300">{formatDate(task.updated_at)}</span>
              </div>
              {task.completed_at && (
                <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                  <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500" />
                  <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.completedAt' })}</span>
                  <span className="ml-auto text-stone-700 dark:text-stone-300">{formatDate(task.completed_at)}</span>
                </div>
              )}
              {task.parent_task_id && (
                <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                  <Link2 className="h-3.5 w-3.5" />
                  <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.parentTask' })}</span>
                  <span className="ml-auto font-mono text-stone-700 dark:text-stone-300">{task.parent_task_id}</span>
                </div>
              )}
              {task.message_id && (
                <div className="flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                  <Link2 className="h-3.5 w-3.5" />
                  <span className="font-medium">{intl.formatMessage({ id: 'tasks.detail.messageId' })}</span>
                  <span className="ml-auto font-mono text-stone-700 dark:text-stone-300">{task.message_id}</span>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

// ── Main Page ───────────────────────────────────────────────

export function TaskBoardPage() {
  const intl = useIntl();
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
  const { updateTask } = useTasksStore();
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<TaskInfo | null>(null);
  const [detailTarget, setDetailTarget] = useState<TaskInfo | null>(null);

  useEffect(() => {
    fetchTasks();
    fetchAgents();
  }, [fetchTasks, fetchAgents]);

  const handleCreate = useCallback(
    async (params: TaskCreateParams) => {
      await createTask(params);
    },
    [createTask],
  );

  const handleDrop = useCallback(
    (taskId: string, newStatus: TaskStatus) => {
      moveTask(taskId, newStatus);
    },
    [moveTask],
  );

  const handleRemoveConfirm = useCallback(async () => {
    if (removeTarget) {
      await removeTask(removeTarget.id);
      setRemoveTarget(null);
      // The detail panel may still reference the task we just deleted.
      setDetailTarget((prev) => (prev?.id === removeTarget.id ? null : prev));
    }
  }, [removeTarget, removeTask]);

  // Apply filters
  const filteredTasks = tasks.filter((t) => {
    if (filterAgent && t.assigned_to !== filterAgent) return false;
    if (filterPriority && t.priority !== filterPriority) return false;
    return true;
  });

  const tasksByStatus = (status: TaskStatus) => filteredTasks.filter((t) => t.status === status);

  return (
    <Page wide>
      <PageHeader
        icon={KanbanSquare}
        title={intl.formatMessage({ id: 'nav.tasks' })}
        subtitle={intl.formatMessage({ id: 'tasks.title' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowCreateDialog(true)}>
            {intl.formatMessage({ id: 'tasks.create' })}
          </Button>
        }
      />

      {/* Filters */}
      <Toolbar>
        <Filter className="h-4 w-4 text-stone-400" />
        <select
          className={cn(controlClass, 'w-auto min-w-[140px]')}
          value={filterAgent ?? ''}
          onChange={(e) => setFilterAgent(e.target.value || null)}
        >
          <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })} — {intl.formatMessage({ id: 'tasks.filter.agent' })}</option>
          {agents.map((a) => (
            <option key={a.name} value={a.name}>
              {a.icon || '🤖'} {a.display_name}
            </option>
          ))}
        </select>
        <select
          className={cn(controlClass, 'w-auto min-w-[140px]')}
          value={filterPriority ?? ''}
          onChange={(e) => setFilterPriority((e.target.value as TaskPriority) || null)}
        >
          <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })} — {intl.formatMessage({ id: 'tasks.field.priority' })}</option>
          {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
            <option key={p} value={p}>
              {intl.formatMessage({ id: `tasks.priority.${p}` })}
            </option>
          ))}
        </select>
      </Toolbar>

      {/* Empty hint (only when truly empty and loaded) */}
      {tasks.length === 0 && !loading && (
        <Card padded={false}>
          <EmptyState
            icon={Clock}
            title={intl.formatMessage({ id: 'tasks.empty' })}
          />
        </Card>
      )}

      {/* Kanban Board — always 4 columns, matches original Multica design */}
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-4">
        {COLUMNS.map(({ status, icon }) => (
          <KanbanColumn
            key={status}
            status={status}
            icon={icon}
            tasks={tasksByStatus(status)}
            agents={agents}
            onDrop={handleDrop}
            onRemove={(id) => {
              const t = tasks.find((task) => task.id === id);
              if (t) setRemoveTarget(t);
            }}
            onSelect={setDetailTarget}
          />
        ))}
      </div>

      {/* Create Dialog */}
      <CreateTaskDialog
        open={showCreateDialog}
        onClose={() => setShowCreateDialog(false)}
        agents={agents}
        onCreate={handleCreate}
      />

      {/* Task Detail Panel */}
      {detailTarget && (
        <TaskDetailPanel
          task={detailTarget}
          agents={agents}
          onClose={() => setDetailTarget(null)}
          onUpdate={async (taskId, fields) => {
            await updateTask(taskId, fields);
            setDetailTarget(null);
          }}
        />
      )}

      {/* Remove Confirmation Dialog */}
      <Dialog
        open={removeTarget !== null}
        title={intl.formatMessage({ id: 'tasks.remove' })}
        onClose={() => setRemoveTarget(null)}
      >
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
    </Page>
  );
}
