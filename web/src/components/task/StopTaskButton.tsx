import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/mds';
import { api, type TaskStopStatus } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';

/** The sentence for a stop state the server reported. */
export function stopStateMessageId(stop: TaskStopStatus): string {
  switch (stop.state) {
    case 'stopped':
      return 'tasks.stop.state.stopped';
    case 'stopped_uncertain':
      return 'tasks.stop.state.uncertain';
    default:
      return stop.detail.unverified_work_possible
        ? 'tasks.stop.state.pendingUnverified'
        : 'tasks.stop.state.pending';
  }
}

/** The server's error code (the text before the first `:`), if any. */
export function errorCode(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e ?? '');
  const m = /^([a-z_]+):/.exec(msg.trim());
  return m ? m[1] : '';
}

/**
 * P2-A — stop a task and its sub-tasks. Two confirmations; the state shown is
 * only ever what the server reported, and a response that arrives after the
 * page switched to another task is dropped.
 *
 * The revision sent is the one read when the dialog opened (what the user
 * was looking at). If the task changed since, the server refuses; the dialog
 * then says so, reads the new revision and waits for the user to confirm
 * again. It never resends on its own.
 */
export function StopTaskButton({
  taskId,
  terminal,
  onStopped,
}: {
  taskId: string;
  terminal: boolean;
  onStopped?: () => void;
}) {
  const intl = useIntl();
  const current = useRef(taskId);
  const [stop, setStop] = useState<TaskStopStatus | null>(null);
  const [step, setStep] = useState<0 | 1 | 2>(0);
  const [busy, setBusy] = useState(false);
  const [seenRevision, setSeenRevision] = useState<number | null>(null);
  const [changed, setChanged] = useState(false);

  const refresh = useCallback(async () => {
    const id = taskId;
    try {
      const r = await api.tasks.stopStatus(id);
      if (current.current !== id) return;
      setStop(r?.stop ?? null);
    } catch {
      // Status is informational; the button still works without it.
    }
  }, [taskId]);

  useEffect(() => {
    current.current = taskId;
    setStop(null);
    setStep(0);
    setBusy(false);
    setSeenRevision(null);
    setChanged(false);
    void refresh();
  }, [taskId, refresh]);

  /** Read the revision the user is about to confirm against. */
  const readRevision = useCallback(async () => {
    const id = taskId;
    try {
      const r = await api.tasks.stopStatus(id);
      if (current.current !== id) return;
      if (r?.stop) {
        setStop(r.stop);
        setStep(0);
        return;
      }
      setSeenRevision(typeof r?.authority_revision === 'number' ? r.authority_revision : null);
    } catch (e) {
      if (current.current !== id) return;
      setSeenRevision(null);
      toast.error(intl.formatMessage({ id: 'tasks.stop.loadFailed' }, { message: formatError(e) }));
    }
  }, [taskId, intl]);

  const open = useCallback(() => {
    setChanged(false);
    setSeenRevision(null);
    setStep(1);
    void readRevision();
  }, [readRevision]);

  const confirm = useCallback(async () => {
    const id = taskId;
    if (seenRevision === null) return;
    setBusy(true);
    try {
      const r = await api.tasks.stop(id, seenRevision);
      if (current.current !== id) return;
      setStop(r?.stop ?? null);
      setStep(0);
      setChanged(false);
      onStopped?.();
    } catch (e) {
      if (current.current !== id) return;
      const code = errorCode(e);
      if (code === 'conflict') {
        // Show the change and wait for a new confirmation with the new revision.
        setChanged(true);
        setSeenRevision(null);
        void readRevision();
      } else if (code === 'already_finished') {
        setStep(0);
        toast.error(intl.formatMessage({ id: 'tasks.stop.alreadyFinished' }));
      } else {
        toast.error(intl.formatMessage({ id: 'tasks.stop.failed' }, { message: formatError(e) }));
      }
    } finally {
      if (current.current === id) setBusy(false);
    }
  }, [taskId, seenRevision, intl, onStopped, readRevision]);

  if (stop) {
    return (
      <div className="flex flex-wrap items-center gap-2" role="status">
        <span className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: stopStateMessageId(stop) })}
        </span>
        {stop.state === 'cancel_pending' && (
          <Button variant="ghost" size="sm" onClick={() => void refresh()}>
            {intl.formatMessage({ id: 'tasks.stop.recheck' })}
          </Button>
        )}
      </div>
    );
  }
  if (terminal) return null;

  return (
    <>
      <Button variant="outline" size="sm" onClick={open}>
        {intl.formatMessage({ id: 'tasks.stop.button' })}
      </Button>
      <Dialog open={step > 0} onOpenChange={(o) => !o && !busy && setStep(0)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>
              {intl.formatMessage({ id: step === 2 ? 'tasks.stop.confirmTitle' : 'tasks.stop.title' })}
            </DialogTitle>
            <DialogDescription>
              {intl.formatMessage({ id: step === 2 ? 'tasks.stop.confirmBody' : 'tasks.stop.body' })}
            </DialogDescription>
          </DialogHeader>
          {changed && (
            <p role="alert" className="text-xs text-amber-700 dark:text-amber-400">
              {intl.formatMessage({ id: 'tasks.stop.changed' })}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setStep(0)}>
              {intl.formatMessage({ id: 'tasks.stop.cancel' })}
            </Button>
            {step === 1 ? (
              <Button variant="destructive" onClick={() => setStep(2)}>
                {intl.formatMessage({ id: 'tasks.stop.next' })}
              </Button>
            ) : (
              <Button
                variant="destructive"
                disabled={busy || seenRevision === null}
                onClick={() => void confirm()}
              >
                {intl.formatMessage({ id: 'tasks.stop.confirm' })}
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
