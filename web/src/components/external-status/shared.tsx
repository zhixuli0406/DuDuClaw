import { useIntl, type IntlShape } from 'react-intl';
import {
  Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/mds';
import type { StatusTokenErrorCode } from '@/lib/api';

const KNOWN_CODES: readonly StatusTokenErrorCode[] = [
  'external_status_disabled',
  'status_token_invalid_name',
  'status_token_limit_reached',
  'status_token_account_inactive',
  'status_token_not_found',
  'status_token_unavailable',
];

/** The structured `error.code` of a rejected status-token RPC, if it is one we know. */
export function statusTokenErrorCode(err: unknown): StatusTokenErrorCode | undefined {
  const code = err && typeof err === 'object' ? (err as { code?: unknown }).code : undefined;
  return KNOWN_CODES.find(known => known === code);
}

/** Friendly localized text for a status-token RPC failure (never the raw server string). */
export function statusTokenErrorMessage(intl: IntlShape, err: unknown): string {
  const code = statusTokenErrorCode(err);
  return intl.formatMessage({ id: code ? `externalStatus.error.${code}` : 'externalStatus.error.generic' });
}

export function formatTokenDate(intl: IntlShape, iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleDateString(intl.locale);
}

export function formatTokenLastUsed(intl: IntlShape, iso: string | null): string {
  if (!iso) return intl.formatMessage({ id: 'externalStatus.neverUsed' });
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString(intl.locale);
}

/** Shared red confirm dialog for the self and admin revoke flows. */
export function RevokeConfirmDialog({ title, body, busy, error, onCancel, onConfirm }: {
  title: string;
  body: string;
  busy: boolean;
  error?: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const intl = useIntl();
  return <Dialog open onOpenChange={open => { if (!open && !busy) onCancel(); }}>
    <DialogContent className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{body}</DialogDescription>
      </DialogHeader>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button variant="outline" disabled={busy} onClick={onCancel}>{intl.formatMessage({ id: 'externalStatus.createDialog.cancel' })}</Button>
        <Button variant="destructive" disabled={busy} onClick={onConfirm}>{intl.formatMessage({ id: 'externalStatus.revokeDialog.confirm' })}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
