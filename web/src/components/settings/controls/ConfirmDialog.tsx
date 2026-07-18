import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  Button,
  Input,
} from '@/components/mds';

/**
 * ConfirmDialog — the single, site-wide destructive-action confirmation. Every
 * delete / offboard / remove routes through this instead of window.confirm or
 * unconfirmed buttons. Optionally requires the user to type an exact string
 * (e.g. the item's name) before the confirm button enables.
 *
 * Internally built on the MDS Dialog; the external API is unchanged so the many
 * consumers do not need edits.
 */
export function ConfirmDialog({
  open,
  onClose,
  onConfirm,
  title,
  message,
  confirmLabel,
  cancelLabel,
  requireText,
  requireTextHint,
  busy,
}: {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** When set, the confirm button stays disabled until the user types this. */
  requireText?: string;
  requireTextHint?: string;
  busy?: boolean;
}) {
  const intl = useIntl();
  const [typed, setTyped] = useState('');

  // Reset the typed buffer each time the dialog (re)opens.
  useEffect(() => {
    if (open) setTyped('');
  }, [open]);

  const canConfirm = !busy && (!requireText || typed.trim() === requireText.trim());

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
        </DialogHeader>

        <div className="flex items-start gap-3">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-destructive" />
          <p className="text-sm text-muted-foreground">{message}</p>
        </div>

        {requireText && (
          <div className="space-y-1.5">
            {requireTextHint && (
              <p className="text-xs text-muted-foreground">{requireTextHint}</p>
            )}
            <Input
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder={requireText}
              autoFocus
            />
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {cancelLabel ?? intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="destructive" onClick={onConfirm} disabled={!canConfirm}>
            {busy
              ? intl.formatMessage({ id: 'common.saving' })
              : confirmLabel ?? intl.formatMessage({ id: 'common.delete' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
