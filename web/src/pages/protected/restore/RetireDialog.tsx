import { useEffect, useId, useState } from 'react';
import { CheckCircle2 } from 'lucide-react';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { adminRestoreApi, type QuarantineRow } from '@/lib/isolation-restore-api';
import { RestoreErrorNotice, useRestoreT } from './restore-ui';
import { isStepUpCancelled, type WithToken } from './with-token';

/** 釋放執行環境 — after a purge, release the employee's runtime on the host.
 *  Its host identity number is never given to another employee. */
export function RetireDialog({
  row,
  open,
  onOpenChange,
  withToken,
  onDone,
}: {
  row: QuarantineRow | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  withToken: WithToken;
  onDone: () => void;
}) {
  const t = useRestoreT();
  const ackId = useId();
  const [ack, setAck] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [sent, setSent] = useState(false);

  const key = row ? `${row.employee_id}:${row.incarnation}` : '';
  useEffect(() => {
    if (open) {
      setAck(false);
      setError(null);
      setSent(false);
    }
  }, [open, key]);

  if (!row) return null;
  const name = row.display_name || row.label;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await withToken((token) => adminRestoreApi.retirePropose(row.employee_id, token));
      setSent(true);
      onDone();
    } catch (e) {
      if (!isStepUpCancelled(e)) setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('restoreFlow.retire.title', { name })}</DialogTitle>
          <DialogDescription>{t('restoreFlow.retire.desc')}</DialogDescription>
        </DialogHeader>
        <p className="text-sm">{t('restoreFlow.retire.neverReused')}</p>
        {sent ? (
          <p role="status" className="flex items-start gap-2 text-sm">
            <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
            {t('restoreFlow.retire.sent')}
          </p>
        ) : (
          <>
            <label htmlFor={ackId} className="flex items-start gap-2 text-sm">
              <input id={ackId} type="checkbox" className="mt-0.5 size-4 accent-primary" checked={ack} onChange={(e) => setAck(e.target.checked)} />
              {t('restoreFlow.retire.ack')}
            </label>
            <RestoreErrorNotice error={error} />
          </>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t(sent ? 'restoreFlow.common.close' : 'restoreFlow.common.cancel')}
          </Button>
          {!sent && (
            <Button variant="destructive" disabled={busy || !ack} onClick={() => void submit()}>
              {busy ? t('restoreFlow.common.sending') : t('restoreFlow.retire.submit')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
