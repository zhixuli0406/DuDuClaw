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
import { adminRestoreApi, type PurgeReply, type QuarantineRow } from '@/lib/isolation-restore-api';
import { errorCode } from '@/lib/restore-errors';
import { EARLY_PURGE_LOST, ItemList, RestoreErrorNotice, useRestoreT } from './restore-ui';
import { isStepUpCancelled, type WithToken } from './with-token';

export type PurgeMode = 'ordinary' | 'early' | 'confirm_early';

/**
 * Purge a quarantined employee's kept data.
 * - `ordinary`: the 90-day window is over; optionally release the host
 *   resources at the same time (同時釋放主機資源).
 * - `early`: before the window ends — lists what will be lost and needs a
 *   second Admin, so this only files the request.
 * - `confirm_early`: the second Admin agrees to someone else's early purge.
 */
export function PurgeDialog({
  row,
  mode,
  open,
  onOpenChange,
  withToken,
  onDone,
}: {
  row: QuarantineRow | null;
  mode: PurgeMode;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  withToken: WithToken;
  onDone: () => void;
}) {
  const t = useRestoreT();
  const ackId = useId();
  const retireId = useId();
  const [ack, setAck] = useState(false);
  const [retire, setRetire] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [reply, setReply] = useState<PurgeReply | null>(null);
  const [serverLost, setServerLost] = useState<string[] | null>(null);

  // Reset per opening (not when a reload hands in a fresh row object).
  const key = row ? `${row.employee_id}:${row.incarnation}` : '';
  useEffect(() => {
    if (open) {
      setAck(false);
      setRetire(false);
      setError(null);
      setReply(null);
      setServerLost(null);
    }
  }, [open, key, mode]);

  if (!row) return null;
  const name = row.display_name || row.label;
  const early = mode !== 'ordinary';
  const lost = serverLost ?? row.purge?.early_purge?.lost ?? EARLY_PURGE_LOST;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const res = await withToken((token) =>
        adminRestoreApi.purge(row.employee_id, row.incarnation, token, early ? { confirmEarly: true } : { retire }),
      );
      setReply(res);
      if (res.lost) setServerLost(res.lost);
      onDone();
    } catch (e) {
      if (isStepUpCancelled(e)) return;
      // The server lists what would be lost when it wants an explicit confirmation.
      const listed = (e as { lost?: unknown })?.lost;
      if (errorCode(e) === 'EARLY_PURGE_CONFIRM_REQUIRED' && Array.isArray(listed)) setServerLost(listed.map(String));
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const title = t(`restoreFlow.purge.${mode}.title`, { name });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{t(`restoreFlow.purge.${mode}.desc`)}</DialogDescription>
        </DialogHeader>

        {reply ? (
          <div role="status" className="space-y-2 text-sm">
            <p className="flex items-start gap-2">
              <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
              {t(reply.state === 'purged' ? 'restoreFlow.purge.done' : 'restoreFlow.purge.requested')}
            </p>
            {reply.state === 'requested' && reply.lost && reply.lost.length > 0 && <ItemList keys={reply.lost} />}
          </div>
        ) : (
          <>
            {early && (
              <section className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3">
                <h3 className="text-sm font-medium">{t('restoreFlow.purge.lost')}</h3>
                <ItemList keys={lost} />
                <p className="text-sm">{t('restoreFlow.purge.noUndo')}</p>
              </section>
            )}
            {mode === 'early' && <p className="text-sm">{t('restoreFlow.purge.secondAdmin')}</p>}
            {mode === 'ordinary' && (
              <label htmlFor={retireId} className="flex items-start gap-2 text-sm">
                <input id={retireId} type="checkbox" className="mt-0.5 size-4 accent-primary" checked={retire}
                  onChange={(e) => setRetire(e.target.checked)} />
                <span>
                  <span className="font-medium">{t('restoreFlow.purge.retire')}</span>
                  <span className="block text-muted-foreground">{t('restoreFlow.purge.retireHint')}</span>
                </span>
              </label>
            )}
            <label htmlFor={ackId} className="flex items-start gap-2 text-sm">
              <input id={ackId} type="checkbox" className="mt-0.5 size-4 accent-primary" checked={ack} onChange={(e) => setAck(e.target.checked)} />
              {t('restoreFlow.purge.ack')}
            </label>
            <RestoreErrorNotice error={error} />
          </>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t(reply ? 'restoreFlow.common.close' : 'restoreFlow.common.cancel')}
          </Button>
          {!reply && (
            <Button variant="destructive" disabled={busy || !ack} onClick={() => void submit()}>
              {busy ? t('restoreFlow.common.sending') : t(`restoreFlow.purge.${mode}.submit`)}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
