import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Dialog, inputClass, buttonSecondary } from '@/components/shared/Dialog';
import { cn } from '@/lib/utils';

/**
 * ConfirmDialog — the single, site-wide destructive-action confirmation. Every
 * delete / offboard / remove routes through this instead of window.confirm or
 * unconfirmed buttons. Optionally requires the user to type an exact string
 * (e.g. the item's name) before the confirm button enables.
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
    <Dialog open={open} onClose={onClose} title={title}>
      <div className="space-y-4">
        <div className="flex items-start gap-3">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-rose-500" />
          <p className="text-sm text-stone-600 dark:text-stone-300">{message}</p>
        </div>

        {requireText && (
          <div className="space-y-1.5">
            {requireTextHint && (
              <p className="text-xs text-stone-500 dark:text-stone-400">{requireTextHint}</p>
            )}
            <input
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder={requireText}
              className={inputClass}
              autoFocus
            />
          </div>
        )}

        <div className="flex justify-end gap-2 pt-1">
          <button onClick={onClose} className={buttonSecondary} type="button">
            {cancelLabel ?? intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={onConfirm}
            disabled={!canConfirm}
            type="button"
            className={cn(
              'inline-flex items-center justify-center gap-2 rounded-lg bg-gradient-to-b from-rose-500 to-rose-600 px-4 py-2 text-sm font-medium text-white shadow-[0_4px_14px_-4px_rgba(244,63,94,0.55)] transition-all hover:from-rose-500 hover:to-rose-700 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50',
            )}
          >
            {busy
              ? intl.formatMessage({ id: 'common.saving' })
              : confirmLabel ?? intl.formatMessage({ id: 'common.delete' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
