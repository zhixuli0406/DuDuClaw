import { useEffect, useId, useState } from 'react';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from '@/components/mds';
import {
  adminRestoreApi,
  isValidEmployeeLabel,
  rootStateOf,
  type QuarantineRow,
} from '@/lib/isolation-restore-api';
import { RestoreErrorNotice, usePersonLabel, useRestoreT } from './restore-ui';
import { isStepUpCancelled, type WithToken } from './with-token';

/**
 * The owner had no second factor before the tombstone, so they cannot prove
 * a request themselves. An Admin files it for them; the request carries
 * 主人未用兩步驟驗證確認 and only a different Admin can approve it.
 */
export function FileForOwnerDialog({
  row,
  open,
  onOpenChange,
  withToken,
  onFiled,
}: {
  row: QuarantineRow | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  withToken: WithToken;
  onFiled: () => void;
}) {
  const t = useRestoreT();
  const labelId = useId();
  const ackId = useId();
  const [label, setLabel] = useState('');
  const [ack, setAck] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const person = usePersonLabel();

  useEffect(() => {
    if (open && row) {
      setLabel(row.label);
      setAck(false);
      setError(null);
    }
  }, [open, row]);

  if (!row) return null;
  const adopt = rootStateOf(row) === 'root_gone';
  const name = row.display_name || row.label;
  const owner = person(row.owner_user_id, row.owner_name);
  const labelOk = !adopt || isValidEmployeeLabel(label);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await withToken((token) =>
        adopt
          ? adminRestoreApi.adoptionForOwner(row.employee_id, row.incarnation, token, label !== row.label ? label : undefined)
          : adminRestoreApi.restoreForOwner(row.employee_id, row.incarnation, token),
      );
      onFiled();
      onOpenChange(false);
    } catch (e) {
      if (!isStepUpCancelled(e)) setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90svh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t(adopt ? 'restoreFlow.forOwner.adoptTitle' : 'restoreFlow.forOwner.title', { name })}</DialogTitle>
          <DialogDescription>{t('restoreFlow.forOwner.desc', { owner })}</DialogDescription>
        </DialogHeader>
        <p className="rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">{t('restoreFlow.forOwner.marked')}</p>
        {adopt && (
          <div className="grid gap-1.5">
            <label htmlFor={labelId} className="text-sm font-medium">{t('restoreFlow.adopt.newLabel')}</label>
            <Input id={labelId} value={label} maxLength={64} spellCheck={false} className="font-mono sm:w-72"
              aria-invalid={!labelOk} onChange={(e) => setLabel(e.target.value.trim().toLowerCase())} />
            <p className={labelOk ? 'text-xs text-muted-foreground' : 'text-xs text-destructive'}>{t('myPersonal.new.labelRule')}</p>
          </div>
        )}
        <label htmlFor={ackId} className="flex items-start gap-2 text-sm">
          <input id={ackId} type="checkbox" className="mt-0.5 size-4 accent-primary" checked={ack} onChange={(e) => setAck(e.target.checked)} />
          {t('restoreFlow.forOwner.ack', { owner })}
        </label>
        <RestoreErrorNotice error={error} />
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>{t('restoreFlow.common.cancel')}</Button>
          <Button variant="brand" disabled={busy || !ack || !labelOk} onClick={() => void submit()}>
            {busy ? t('restoreFlow.common.sending') : t('restoreFlow.forOwner.submit')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
