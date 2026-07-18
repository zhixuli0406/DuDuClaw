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

  // Reset the form each time the modal opens. Default to UNASSIGNED (empty) so
  // a manually-organised task is not silently handed to the first agent and
  // auto-dispatched (Bug#4). A subtask still inherits its parent's assignee via
  // `defaultAssignee` when one is provided.
  useEffect(() => {
    if (open) {
      setTitle('');
      setDescription('');
      setPriority('medium');
      setAssignedTo(defaultAssignee ?? '');
    }
  }, [open, defaultAssignee]);

  const handleSubmit = useCallback(async () => {
    if (!title.trim()) return;
    setSubmitting(true);
    try {
      await onCreate({
        title: title.trim(),
        description: description.trim() || undefined,
        // Omit when unassigned — the gateway treats a missing assignee as an
        // unassigned (never auto-dispatched) task.
        ...(assignedTo ? { assigned_to: assignedTo } : {}),
        priority,
        ...(parentTaskId ? { parent_task_id: parentTaskId } : {}),
      });
      onClose();
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
              <AssigneePopover agents={agents} value={assignedTo || null} onChange={setAssignedTo} allowUnassigned />
            </div>
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
