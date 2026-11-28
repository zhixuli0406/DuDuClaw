import { useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { memberGoogleSignin, type MemberGoogleSigninStatus } from '@/lib/google-signin';
import { settingsRefusal } from '@/lib/settings-refusal';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Input, useErrorMessage } from '@/components/mds';

export function GoogleSigninCard() {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `memberGoogleSignin.${key}` });
  const errorText = useErrorMessage();
  const [status, setStatus] = useState<MemberGoogleSigninStatus | null>(null);
  const [error, setError] = useState('');
  const [dialog, setDialog] = useState<'link' | 'unlink' | null>(null);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const initialized = useRef(false);
  const lock = useRef(false);
  const passwordId = useId();

  useEffect(() => {
    if (initialized.current) return;
    initialized.current = true;
    const match = /^#gsi_link=([A-Za-z0-9_-]+)$/.exec(window.location.hash);
    // Consume the callback before any async work; never retain the ticket in history or state.
    if (match) window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search);
    void (async () => {
      if (match) {
        lock.current = true; setBusy(true);
        try { await memberGoogleSignin.linkComplete(match[1]); }
        catch (err) { setError(settingsRefusal(err, errorText(err))); }
      }
      try { setStatus(await memberGoogleSignin.status()); }
      catch (err) { setError(current => current || settingsRefusal(err, errorText(err))); }
      finally { lock.current = false; setBusy(false); }
    })();
  }, [errorText]);

  const attempt = async () => {
    if (lock.current || !password || !dialog) return;
    lock.current = true; setBusy(true); setError('');
    const currentPassword = password;
    setPassword('');
    try {
      if (dialog === 'link') {
        const result = await memberGoogleSignin.linkStart(currentPassword);
        let url: URL;
        try { url = new URL(result.authorization_url); }
        catch { setError(t('invalidUrl')); return; }
        if (url.origin !== 'https://accounts.google.com' || url.username || url.password) {
          setError(t('invalidUrl')); return;
        }
        setDialog(null);
        window.location.assign(url.href);
      } else {
        await memberGoogleSignin.unlink(currentPassword);
        setDialog(null);
        setStatus(await memberGoogleSignin.status());
      }
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setPassword(''); lock.current = false; setBusy(false); }
  };
  if (status && !status.available && !status.linked) return null;
  if (!status && !error) return null;
  return <Card>
    <CardHeader className="flex-row items-center justify-between gap-3">
      <CardTitle>{t('title')}</CardTitle>
      {status?.linked && <Badge variant="secondary">{t('enabled')}</Badge>}
    </CardHeader>
    <CardContent className="space-y-3">
      {status && (status.linked ? <>
        <p className="break-words text-sm">{status.email_at_link}</p>
        <Button variant="outline" disabled={busy || !status.has_password} onClick={() => { setError(''); setDialog('unlink'); }}>{t('unlink')}</Button>
        <p className="text-sm text-muted-foreground">{t(status.has_password ? 'unlinkNote' : 'noPassword')}</p>
      </> : <>
        <p className="text-sm text-muted-foreground">{t('intro')}</p>
        <Button disabled={busy} onClick={() => { setError(''); setDialog('link'); }}>{t('link')}</Button>
        <p className="text-sm text-muted-foreground">{t('linkNote')}</p>
      </>)}
      {error && !dialog && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <Dialog open={dialog !== null} onOpenChange={open => { if (!open && !lock.current) { setDialog(null); setPassword(''); } }}>
        <DialogContent>
          <form onSubmit={event => { event.preventDefault(); void attempt(); }} className="space-y-4">
            <DialogHeader>
              <DialogTitle>{t('confirmTitle')}</DialogTitle>
              <DialogDescription>{t(dialog === 'unlink' ? 'unlinkNote' : 'confirmDescription')}</DialogDescription>
            </DialogHeader>
            <div className="space-y-2">
              <label htmlFor={passwordId} className="text-sm">{t('password')}</label>
              <Input id={passwordId} type="password" autoComplete="current-password" value={password} disabled={busy} onChange={event => setPassword(event.target.value)} />
            </div>
            {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button type="button" variant="outline" disabled={busy} onClick={() => { setDialog(null); setPassword(''); }}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
              <Button type="submit" disabled={busy || !password}>{t(dialog === 'unlink' ? 'unlink' : 'goGoogle')}</Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </CardContent>
  </Card>;
}
