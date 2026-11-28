import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Users } from 'lucide-react';
import { Button, Empty, useErrorMessage } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { googleSignin, signinRequests, type SigninRequest } from '@/lib/google-signin';
import { formatDateTime } from '@/lib/company-time';
import { settingsRefusal } from '@/lib/settings-refusal';

export function useSigninRequests(isAdmin: boolean) {
  const errorText = useErrorMessage();
  const [requests, setRequests] = useState<SigninRequest[]>([]);
  const [available, setAvailable] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    if (!isAdmin) return;
    try {
      const result = await signinRequests.list();
      setRequests(result.requests ?? []);
      setAvailable(true);
      setError(null);
    } catch (err) {
      const code = err && typeof err === 'object' && 'code' in err ? err.code : null;
      if (code === 'signin_personal_edition' || code === 'signin_admin_required') {
        setAvailable(false);
        setRequests([]);
        setError(null);
      } else {
        setError(settingsRefusal(err, errorText(err)));
      }
    }
  }, [isAdmin, errorText]);
  useEffect(() => { void refresh(); }, [refresh]);
  return { requests, available: isAdmin && available, error: isAdmin ? error : null, refresh };
}

export function SigninRequestsPanel({ requests, onChanged }: {
  requests: SigninRequest[];
  onChanged: () => Promise<void>;
}) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const t = (key: string) => intl.formatMessage({ id: `users.signinRequests.${key}` });
  const [selection, setSelection] = useState<{ request: SigninRequest; action: 'approve' | 'reject' } | null>(null);
  const [allowMfa, setAllowMfa] = useState(false);
  const [mfa, setMfa] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    googleSignin.get().then(result => { if (active) setAllowMfa(result.settings.approval_grants_mfa_enrollment === true); })
      .catch(err => { if (active) setError(settingsRefusal(err, errorText(err))); });
    return () => { active = false; };
  }, [errorText]);
  const choose = (request: SigninRequest, action: 'approve' | 'reject') => {
    setSelection({ request, action }); setMfa(false); setError(null);
  };
  const confirm = async () => {
    if (!selection || busy) return;
    setBusy(true); setError(null);
    try {
      if (selection.action === 'approve') await signinRequests.approve(selection.request.id, allowMfa && mfa);
      else await signinRequests.reject(selection.request.id);
      setSelection(null);
      await onChanged();
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  };
  const errorNotice = error && <p role="alert" className="text-sm text-destructive">{error}</p>;
  return <div className="space-y-4">
    {!selection && errorNotice}
    {requests.length === 0 ? <Empty icon={Users} title={t('empty')} description={t('emptyHint')} /> :
      <div className="overflow-x-auto rounded-xl border border-surface-border">
        <table className="w-full text-left text-sm">
          <thead className="bg-muted text-xs text-muted-foreground"><tr>
            {['name', 'googleAccount', 'requestedAt', 'expiresAt', 'actions'].map(key => <th key={key} scope="col" className="px-4 py-3 font-medium">{t(key)}</th>)}
          </tr></thead>
          <tbody>{requests.map(request => <tr key={request.id} className="border-t border-surface-border">
            <td className="px-4 py-3 font-medium">{request.display_name}</td>
            <td className="px-4 py-3"><p>{request.email}</p><p className="text-xs text-muted-foreground">{request.hd}</p></td>
            <td className="whitespace-nowrap px-4 py-3 tabular-nums">{formatDateTime(request.requested_at * 1000, { locale: intl.locale })}</td>
            <td className="whitespace-nowrap px-4 py-3 text-muted-foreground">{intl.formatMessage({ id: 'users.signinRequests.expiry' }, {
              days: Math.max(0, Math.ceil((request.expires_at * 1000 - Date.now()) / 86400000)),
              date: formatDateTime(request.expires_at * 1000, { month: 'numeric', day: 'numeric', locale: intl.locale }),
            })}</td>
            <td className="px-4 py-3"><div className="flex gap-2">
              <Button size="sm" variant="brand" disabled={busy} onClick={() => choose(request, 'approve')}>{t('approve')}</Button>
              <Button size="sm" variant="outline" disabled={busy} onClick={() => choose(request, 'reject')}>{t('reject')}</Button>
            </div></td>
          </tr>)}</tbody>
        </table>
      </div>}
    <p className="text-xs text-muted-foreground">{t('footer')}</p>
    {selection && <ConfirmDialog open onClose={() => { if (!busy) setSelection(null); }} onConfirm={() => void confirm()}
      busy={busy} destructive={selection.action === 'reject'}
      title={intl.formatMessage({ id: `users.signinRequests.${selection.action}Title` }, { name: selection.request.display_name })}
      message={intl.formatMessage({ id: `users.signinRequests.${selection.action}Body` }, { email: selection.request.email })}
      confirmLabel={t(selection.action)}>
      {selection.action === 'approve' && allowMfa && <label className="flex items-start gap-2 text-sm">
        <input type="checkbox" checked={mfa} disabled={busy} onChange={event => setMfa(event.target.checked)} />
        <span>{t('mfa')}<span className="block text-xs text-muted-foreground">{t('mfaHint')}</span></span>
      </label>}
      {errorNotice}
    </ConfirmDialog>}
  </div>;
}
