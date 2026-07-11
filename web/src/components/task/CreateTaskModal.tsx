import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { Dialog, FormField, inputClass, selectClass } from '@/components/shared/Dialog';
import { Button } from '@/components/ui';
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

  // Reset the form each time the modal opens; seed the assignee.
  useEffect(() => {
    if (open) {
      setTitle('');
      setDescription('');
      setPriority('medium');
      setAssignedTo(defaultAssignee ?? agents[0]?.name ?? '');
    }
  }, [open, defaultAssignee, agents]);

  const handleSubmit = useCallback(async () => {
    if (!title.trim() || !assignedTo) return;
    setSubmitting(true);
    try {
      await onCreate({
        title: title.trim(),
        description: description.trim() || undefined,
        assigned_to: assignedTo,
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
    <Dialog open={open} title={dialogTitle} onClose={onClose}>
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
          <div className="rounded-lg border border-stone-300/70 bg-white/60 px-1 py-1 dark:border-white/10 dark:bg-white/5">
            <AssigneePopover agents={agents} value={assignedTo || null} onChange={setAssignedTo} />
          </div>
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

        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: 'agents.delegate.close' })}
          </Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting || !title.trim() || !assignedTo}>
            {submitting
              ? intl.formatMessage({ id: 'agents.delegate.submitting' })
              : intl.formatMessage({ id: 'tasks.create' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
