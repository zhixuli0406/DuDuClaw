import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Badge, Button, Dialog, DialogContent, DialogHeader, DialogTitle, Input } from '@/components/mds';
import type { UserInviteState } from '@/lib/api';
import { CompanyLogo, useCompanyName } from '@/components/brand/CompanyLogo';
import { useCompanyStore } from '@/stores/company-store';
import { toast } from '@/lib/toast';

/** Shown before the operator creates or reissues the invitation. */
export function InviteEmailPreview({ email }: { email: string }) {
  const intl = useIntl();
  const product = useCompanyName();
  const logo = useCompanyStore((state) => state.company.logo_url);
  return <div className="space-y-2 rounded-lg border border-border bg-muted/40 p-3 text-sm">
    <p className="font-medium">{intl.formatMessage({ id: 'users.invite.preview' })}</p>
    {logo && <CompanyLogo size={48} />}
    <p>{intl.formatMessage({ id: 'users.invite.recipient' }, { email })}</p>
    <p className="whitespace-pre-line">{intl.formatMessage({ id: 'users.invite.emailPreview' }, { product })}</p>
  </div>;
}

/** The bearer link lives only in this mounted dialog, never storage or a toast. */
export function InviteLinkDialog({ link, onClose }: { link: string; onClose: () => void }) {
  const intl = useIntl();
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try { await navigator.clipboard.writeText(link); setCopied(true); }
    catch { toast.error(intl.formatMessage({ id: 'users.invite.copyFailed' })); }
  };
  return <Dialog open onOpenChange={open => !open && onClose()}>
    <DialogContent className="sm:max-w-lg"><DialogHeader><DialogTitle>{intl.formatMessage({ id: 'users.invite.fallbackTitle' })}</DialogTitle></DialogHeader>
      <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'users.invite.fallback' })}</p>
      <Input aria-label={intl.formatMessage({ id: 'users.invite.link' })} value={link} readOnly autoComplete="off" />
      <Button onClick={() => void copy()}>{intl.formatMessage({ id: copied ? 'users.invite.copied' : 'users.invite.copy' })}</Button>
      <Button variant="outline" onClick={onClose}>{intl.formatMessage({ id: 'common.close' })}</Button>
    </DialogContent>
  </Dialog>;
}

export function InviteStatusChip({ state }: { state?: UserInviteState }) {
  const intl = useIntl();
  if (state !== 'pending' && state !== 'expired') return null;
  return <Badge variant="outline">{intl.formatMessage({ id: `users.invite.${state}` })}</Badge>;
}
