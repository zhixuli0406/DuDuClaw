import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { Button, Card, CardContent, CardHeader, CardTitle, useErrorMessage } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { GoogleDataDisclosure } from '@/components/GoogleDataDisclosure';
import { settingsRefusal } from '@/lib/settings-refusal';

type ConnectResult = Awaited<ReturnType<typeof api.personalGoogle.writeConnectStart>>;

export function DriveWriteMemberCard({ status, onRefresh, onStarted }: {
  status: PersonalGoogleStatus;
  onRefresh: () => Promise<void>;
  onStarted: (result: ConnectResult) => void;
}) {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `driveWriteMember.${key}` }, values);
  const errorText = useErrorMessage();
  const [googleData, setGoogleData] = useState(false);
  const [write, setWrite] = useState(false);
  const [busy, setBusy] = useState(false);
  const [remove, setRemove] = useState(false);
  const [error, setError] = useState('');
  const [removed, setRemoved] = useState(false);
  const [note, setNote] = useState('');
  const drive = status.drive_write;
  const visible = status.enabled_by_admin && (status.requests.length > 0 || drive?.connected);
  const different = !!(drive?.account_email && status.connection?.account_email
    && drive.account_email.toLowerCase() !== status.connection.account_email.toLowerCase());
  const unreadable = drive?.status === 'unreadable';
  const linked = drive?.connected && !different;

  const connect = async () => {
    if (busy || !googleData || !write || unreadable) return;
    const popup = window.open('about:blank', '_blank');
    if (popup) popup.opener = null;
    setBusy(true);
    setError('');
    try {
      const started = await api.personalGoogle.writeConnectStart(googleData, write);
      const url = new URL(started.authorization_url);
      if (url.origin !== 'https://accounts.google.com' || url.username || url.password) {
        popup?.close();
        setError(t('invalidUrl'));
        return;
      }
      onStarted(started);
      setGoogleData(false);
      setWrite(false);
      if (popup) popup.location.replace(url.href);
      else window.location.assign(url.href);
    } catch (err) {
      popup?.close();
      setError(settingsRefusal(err, errorText(err)));
    } finally { setBusy(false); }
  };
  const disconnect = async () => {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      const result = await api.personalGoogle.writeDisconnect();
      setNote(result.provider_note ?? '');
      setRemoved(true);
      setRemove(false);
      await onRefresh();
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  };

  // Keep the revocation note visible even if removing the last connection hides the controls.
  if (!visible && !removed) return null;
  return <Card>
    <CardHeader><CardTitle>{t('title')}</CardTitle></CardHeader>
    <CardContent className="space-y-4">
      {visible && (unreadable ? <p role="status">{t('unreadable')}</p> : linked ? <>
        <p className="text-sm text-success">{t('linked')}</p>
        <p className="break-words">{t('account')}: {drive.account_email}</p>
        <Button variant="outline" disabled={busy} onClick={() => setRemove(true)}>{t('disconnect')}</Button>
      </> : <>
        <p className="text-sm text-warning">{t(different ? 'needsReconnect' : 'needsLink')}</p>
        <p className="text-sm text-muted-foreground">{t('intro')}</p>
        {different && <p className="break-words text-sm text-warning">{t('different', { write: drive.account_email!, normal: status.connection!.account_email })}</p>}
        <GoogleDataDisclosure confirmed={googleData} onConfirmedChange={setGoogleData} />
        <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={write} onChange={e => setWrite(e.target.checked)} /><span>{t('acknowledge')}</span></label>
        <Button disabled={busy || !googleData || !write} onClick={() => void connect()}>{t(different ? 'reconnect' : 'connect')}</Button>
      </>)}
      {removed && <div role="status" className="space-y-2 text-sm text-muted-foreground"><p>{t('removed')}</p><a className="underline" href="https://myaccount.google.com/connections" target="_blank" rel="noreferrer">{t('thirdParty')}</a>{note && <p className="whitespace-pre-wrap break-words">{note}</p>}</div>}
      {error && !remove && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <ConfirmDialog open={remove} onClose={() => { if (!busy) setRemove(false); }} onConfirm={() => void disconnect()} busy={busy} title={t('disconnectQuestion')} message={t('disconnectHint')} confirmLabel={t('disconnect')}>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      </ConfirmDialog>
    </CardContent>
  </Card>;
}
