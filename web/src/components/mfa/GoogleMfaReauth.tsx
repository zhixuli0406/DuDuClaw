import { useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, useErrorMessage } from '@/components/mds';
import { memberGoogleSignin } from '@/lib/google-signin';
import { settingsRefusal } from '@/lib/settings-refusal';

export function GoogleMfaReauth() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const errorText = useErrorMessage();
  const lock = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const start = async () => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError('');
    try {
      const result = await memberGoogleSignin.reauthStart();
      let url: URL;
      try { url = new URL(result.authorization_url); }
      catch { setError(t('memberGoogleSignin.invalidUrl')); return; }
      if (url.origin !== 'https://accounts.google.com' || url.username || url.password) {
        setError(t('memberGoogleSignin.invalidUrl')); return;
      }
      window.location.assign(url.href);
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { lock.current = false; setBusy(false); }
  };
  return <div className="space-y-2">
    <p className="text-sm text-muted-foreground">{t('mfa.googleReauth.hint')}</p>
    <Button variant="outline" disabled={busy} onClick={() => void start()}>{t('mfa.googleReauth.start')}</Button>
    {error && <p role="alert" className="text-destructive">{error}</p>}
  </div>;
}
