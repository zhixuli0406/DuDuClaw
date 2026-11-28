import { useEffect, useId, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Button, Checkbox, Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from '@/components/mds';

export interface RiskConfirmDialogProps {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  body: ReactNode;
  acknowledgeLabel?: string;
  auditNote?: string | null;
  confirmLabel?: string;
  cancelLabel?: string;
  busy?: boolean;
  error?: string | null;
}

/** Explicit acknowledgement before enabling a setting with wider effects. */
export function RiskConfirmDialog({ open, onClose, onConfirm, title, body, acknowledgeLabel,
  auditNote, confirmLabel, cancelLabel, busy, error }: RiskConfirmDialogProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [acknowledged, setAcknowledged] = useState(false);
  const checkboxId = useId();
  useEffect(() => { if (open) setAcknowledged(false); }, [open]);

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <DialogContent className="sm:max-w-md" showClose={false}>
        <DialogHeader><DialogTitle>{title}</DialogTitle></DialogHeader>
        <div className="text-sm text-muted-foreground">{body}</div>
        <div className="flex items-center gap-2">
          <Checkbox id={checkboxId} checked={acknowledged} onCheckedChange={setAcknowledged} disabled={busy} />
          <label htmlFor={checkboxId}>{acknowledgeLabel ?? t('riskConfirm.acknowledge')}</label>
        </div>
        {auditNote !== null && <p className="text-xs text-muted-foreground">{auditNote ?? t('riskConfirm.auditNote')}</p>}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{cancelLabel ?? t('common.cancel')}</Button>
          <Button variant="default" disabled={!acknowledged || busy} onClick={onConfirm}>
            {confirmLabel ?? t('riskConfirm.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
