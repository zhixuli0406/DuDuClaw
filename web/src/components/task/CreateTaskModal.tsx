import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@/components/mds';
import { AssigneePopover, type AssigneeOption } from './AssigneePopover';
import type { TaskCreateParams, TaskPriority } from '@/lib/api';
import { formatError, formatErrorDetail } from '@/lib/toast';

/**
 * CreateTaskModal — the one "＋交辦任務" surface (§5.3 T5.1). Opened either from
 * the board's own button or by the `?new=1` query the Sidebar / MobileBottomNav
 * route to. Fields: title / description / assignee (avatar picker) / priority.
 * `parentTaskId` lets the detail page reuse it as the "＋子任務" creator.
 */
export function CreateTaskModal({
  open,
  onClose,
  agents,
  onCreate,
  parentTaskId,
  defaultAssignee,
}: {
  open: boolean;
  onClose: () => void;
  agents: ReadonlyArray<AssigneeOption>;
  onCreate: (params: TaskCreateParams) => Promise<unknown>;
  parentTaskId?: string;
  defaultAssignee?: string;
}) {
  const intl = useIntl();
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [assignedTo, setAssignedTo] = useState('');
  const [priority, setPriority] = useState<TaskPriority>('medium');
  const [submitting, setSubmitting] = useState(false);
  // Shown inline: the missing-assignee hint, or a refused create.
  const [assigneeMissing, setAssigneeMissing] = useState(false);
  const [createError, setCreateError] = useState<unknown>(null);

  // Reset the form each time the modal opens. The assignee starts EMPTY so a
  // task is never silently handed to the first agent and auto-dispatched
  // (Bug#4); the person must pick one, because the gateway refuses a task
  // without `assigned_to` (`tasks.create` in handlers/dispatch_org.rs). A
  // subtask still inherits its parent's assignee via `defaultAssignee`.
  useEffect(() => {
    if (open) {
      setTitle('');
      setDescription('');
      setPriority('medium');
      setAssignedTo(defaultAssignee ?? '');
      setAssigneeMissing(false);
      setCreateError(null);
    }
  }, [open, defaultAssignee]);

  const handleSubmit = useCallback(async () => {
    if (!title.trim()) return;
    if (!assignedTo) {
      setAssigneeMissing(true);
      return;
    }
    setSubmitting(true);
    setCreateError(null);
    try {
      const created = await onCreate({
        title: title.trim(),
        description: description.trim() || undefined,
        assigned_to: assignedTo,
        priority,
        ...(parentTaskId ? { parent_task_id: parentTaskId } : {}),
      });
      // The tasks store answers `null` when the gateway refused the create;
      // keep the dialog open so nothing typed is lost.
      if (created === null) {
        setCreateError(true);
        return;
      }
      onClose();
    } catch (e) {
      setCreateError(e);
    } finally {
      setSubmitting(false);
    }
  }, [title, description, assignedTo, priority, parentTaskId, onCreate, onClose]);

  const dialogTitle = parentTaskId
    ? intl.formatMessage({ id: 'tasks.subtask.add' })
    : intl.formatMessage({ id: 'tasks.create' });

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{dialogTitle}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <ModalField label={intl.formatMessage({ id: 'tasks.field.title' })}>
            <Input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder={intl.formatMessage({ id: 'tasks.field.title' })}
              autoFocus
            />
          </ModalField>

          <ModalField label={intl.formatMessage({ id: 'tasks.field.description' })}>
            <Textarea
              className="min-h-[80px] resize-y"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder={intl.formatMessage({ id: 'tasks.field.description' })}
            />
          </ModalField>

          <ModalField label={intl.formatMessage({ id: 'tasks.field.assignTo' })}>
            <div className="rounded-lg border border-input px-1 py-1">
              <AssigneePopover
                agents={agents}
                value={assignedTo || null}
                onChange={(name) => {
                  setAssignedTo(name);
                  if (name) setAssigneeMissing(false);
                }}
              />
            </div>
            {assigneeMissing && (
              <p role="alert" className="text-xs text-destructive">
                {intl.formatMessage({ id: 'tasks.create.assigneeRequired' })}
              </p>
            )}
          </ModalField>

          <ModalField label={intl.formatMessage({ id: 'tasks.field.priority' })}>
            <Select value={priority} onValueChange={(v) => setPriority(v as TaskPriority)}>
              <SelectTrigger className="w-full">
                <SelectValue>
                  {intl.formatMessage({ id: `tasks.priority.${priority}` })}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {(['low', 'medium', 'high', 'urgent'] as const).map((p) => (
                  <SelectItem key={p} value={p}>
                    {intl.formatMessage({ id: `tasks.priority.${p}` })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </ModalField>
          {createError != null && (
            <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              {createError === true
                ? intl.formatMessage({ id: 'tasks.create.failed' })
                : formatErrorDetail(createError) || formatError(createError)}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'agents.delegate.close' })}
          </Button>
          <Button
            variant="brand"
            onClick={handleSubmit}
            disabled={submitting || !title.trim()}
          >
            {submitting
              ? intl.formatMessage({ id: 'agents.delegate.submitting' })
              : intl.formatMessage({ id: 'tasks.create' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ModalField({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium text-muted-foreground">{label}</label>
      {children}
    </div>
  );
}
