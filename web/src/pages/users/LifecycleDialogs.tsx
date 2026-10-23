// Remove and offboard dialogs of the Users page (moved out of UsersPage.tsx
// unchanged).
import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Button,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  ActorAvatar,
} from '@/components/mds';
import { DialogField, type Option } from './dialog-common';

// ── UI.1 — delete-user confirm dialog ──

export function RemoveUserDialog({
  user,
  onClose,
  onRemoved,
}: {
  user: UserDetail;
  onClose: () => void;
  onRemoved: () => void;
}) {
  const intl = useIntl();
  const [submitting, setSubmitting] = useState(false);

  const handleConfirm = async () => {
    setSubmitting(true);
    try {
      await api.users.remove(user.id);
      toast.success(intl.formatMessage({ id: 'users.removed' }));
      onRemoved();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      setSubmitting(false);
    }
  };

  return (
    <ConfirmDialog
      open
      onClose={onClose}
      onConfirm={handleConfirm}
      title={intl.formatMessage({ id: 'users.action.remove' })}
      message={intl.formatMessage({ id: 'users.remove.confirm' }, { name: user.display_name })}
      confirmLabel={intl.formatMessage({ id: 'common.delete' })}
      requireText={user.display_name}
      requireTextHint={intl.formatMessage({ id: 'users.remove.requireHint' })}
      busy={submitting}
    />
  );
}

export function OffboardDialog({
  user,
  users,
  onClose,
  onOffboarded,
}: {
  user: UserDetail;
  users: UserDetail[];
  onClose: () => void;
  onOffboarded: () => void;
}) {
  const intl = useIntl();
  const [transferTo, setTransferTo] = useState('');
  const [confirming, setConfirming] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const handleConfirm = async () => {
    setSubmitting(true);
    try {
      await api.users.offboard(user.id, transferTo || undefined);
      onOffboarded();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      setSubmitting(false);
    }
  };

  const hasBindings = user.bindings.length > 0;

  // No AI staff to reassign (or transferee already chosen) → route the
  // destructive step through the shared ConfirmDialog. Payload is unchanged:
  // offboard(user.id, transferTo || undefined).
  if (!hasBindings || confirming) {
    return (
      <ConfirmDialog
        open
        onClose={hasBindings ? () => setConfirming(false) : onClose}
        onConfirm={handleConfirm}
        title={intl.formatMessage({ id: 'users.action.offboard' })}
        message={intl.formatMessage({ id: 'users.offboard.confirm' }, { name: user.display_name })}
        confirmLabel={intl.formatMessage({ id: 'users.action.offboard' })}
        busy={submitting}
      />
    );
  }

  // Bindings present → pick a transferee first (preserves the transferTo field),
  // then hand off to ConfirmDialog above.
  const transferOptions: Option[] = [
    { value: '', label: intl.formatMessage({ id: 'users.offboard.no_transfer' }) },
    ...users.map((u) => ({ value: u.id, label: `${u.display_name} (${u.email})` })),
  ];

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.action.offboard' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'users.offboard.confirm' }, { name: user.display_name })}
          </p>
          <div>
            <p className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'users.offboard.agents' })}
            </p>
            <div className="mt-1 flex flex-wrap gap-1">
              {user.bindings.map((b) => (
                <span
                  key={b.agent_name}
                  className="inline-flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                >
                  <ActorAvatar actorType="agent" name={b.agent_name} size="xs" />
                  {b.agent_name}
                </span>
              ))}
            </div>
          </div>
          <DialogField label={intl.formatMessage({ id: 'users.offboard.transfer' })}>
            <Select value={transferTo} onValueChange={(v) => setTransferTo(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{transferOptions.find((o) => o.value === transferTo)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {transferOptions.map((o) => (
                  <SelectItem key={o.value || 'none'} value={o.value}>{o.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="destructive" onClick={() => setConfirming(true)}>
            {intl.formatMessage({ id: 'users.action.offboard' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
