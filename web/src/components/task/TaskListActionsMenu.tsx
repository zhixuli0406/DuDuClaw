import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { MoreHorizontal, Pin, PinOff, Pencil, Archive, ArchiveRestore } from 'lucide-react';
import { api, type TaskInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogClose,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  Input,
} from '@/components/mds';

/**
 * TaskListActionsMenu — I-3b: the pin/rename/archive kebab. Currently used by
 * `/goals`'s waiting/active cards and finished/archived rows; kept as a
 * standalone component (not page-local) since the same three actions apply
 * to any task list, not just goal-mode ones. A thin wrapper over the
 * `tasks.pin` / `tasks.archive` / `tasks.rename` RPCs (`handle_tasks_update`
 * under the hood — same HS4 authorization as every other task write).
 */
export function TaskListActionsMenu({
  task,
  onTogglePin,
  onToggleArchive,
  onRename,
}: {
  task: TaskInfo;
  onTogglePin: () => void;
  onToggleArchive: () => void;
  onRename: () => void;
}) {
  const intl = useIntl();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button variant="ghost" size="icon-sm" aria-label={intl.formatMessage({ id: 'goals.rowMenu.label' })} />
        }
      >
        <MoreHorizontal className="size-3.5" />
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuItem onClick={onTogglePin}>
          {task.pinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
          {intl.formatMessage({ id: task.pinned ? 'goals.action.unpin' : 'goals.action.pin' })}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onRename}>
          <Pencil className="size-3.5" />
          {intl.formatMessage({ id: 'goals.action.rename' })}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={onToggleArchive}>
          {task.archived ? <ArchiveRestore className="size-3.5" /> : <Archive className="size-3.5" />}
          {intl.formatMessage({ id: task.archived ? 'goals.action.unarchive' : 'goals.action.archive' })}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** I-3b: rename dialog — a single required title field, shared by every
 *  section's kebab. */
export function RenameTaskDialog({
  task,
  onClose,
  onRenamed,
}: {
  task: TaskInfo | null;
  onClose: () => void;
  onRenamed: () => void;
}) {
  const intl = useIntl();
  const [title, setTitle] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setTitle(task?.title ?? '');
  }, [task]);

  const submit = async () => {
    if (!task) return;
    const trimmed = title.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      await api.tasks.rename(task.id, trimmed);
      toast.success(intl.formatMessage({ id: 'goals.rename.success' }));
      onRenamed();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={task !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'goals.rename.title' })}</DialogTitle>
        </DialogHeader>
        <Input
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          aria-label={intl.formatMessage({ id: 'goals.rename.title' })}
          autoFocus
          onKeyDown={(e) => {
            if (e.key === 'Enter') void submit();
          }}
        />
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'agents.delegate.close' })}</Button>} />
          <Button variant="brand" disabled={busy || !title.trim()} onClick={() => void submit()}>
            {intl.formatMessage({ id: 'goals.rename.confirm' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
