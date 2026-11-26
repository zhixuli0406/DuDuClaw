import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useLocation } from 'react-router';
import { Button, Card, CardContent, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import { ActorAvatar } from '@/components/mds';
import { GoogleDataDisclosure } from '@/components/GoogleDataDisclosure';
import { googleAuthorization, type GoogleAuthorizationPreview } from '@/lib/google-authorization';

export function GoogleAuthorizationPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string>) => intl.formatMessage({ id }, values);
  const location = useLocation();
  const [ticket] = useState(() => /^#ticket=[a-f0-9]{64}$/.test(location.hash) ? location.hash.substring(8) : '');
  const [preview, setPreview] = useState<GoogleAuthorizationPreview>();
  const [confirmed, setConfirmed] = useState(false);
  const [shared, setShared] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState(false);
  useEffect(() => {
    // Keep the ticket only in component memory after the normal login guard.
    window.history.replaceState(window.history.state, '', window.location.pathname);
    if (!ticket) { setError(true); return; }
    let active = true;
    void googleAuthorization.preview(ticket).then(value => { if (active) setPreview(value); })
      .catch(() => { if (active) setError(true); });
    return () => { active = false; };
  }, [ticket]);
  const confirm = async () => {
    if (!confirmed || !shared || busy) return;
    setBusy(true);
    try {
      const result = await googleAuthorization.confirm(ticket);
      if (result.completed) setDone(true);
      else if (result.authorization_url) {
        // Only the fixed Google authorization endpoint is navigable.
        const url = new URL(result.authorization_url);
        if (url.origin !== 'https://accounts.google.com' || url.pathname !== '/o/oauth2/v2/auth'
          || url.username || url.password || url.hash) throw new Error('invalid authorization destination');
        window.location.assign(url.href);
      } else setError(true);
    } catch { setError(true); }
    finally { setBusy(false); }
  };
  return <div className="mx-auto max-w-2xl space-y-6 p-6">
    <h1 className="text-xl font-semibold">{t('googleAuthorization.title')}</h1>
    <p className="text-sm text-muted-foreground">{t('googleAuthorization.intro')}</p>
    {error ? <p role="alert">{t('googleAuthorization.unavailable')}</p>
      : done ? <p role="status">{t('googleAuthorization.done')}</p>
        : !preview ? <Skeleton className="h-40 w-full" />
          : <Card><CardHeader><CardTitle>{t('googleAuthorization.account', { account: preview.account })}</CardTitle></CardHeader>
            <CardContent className="space-y-4">
              <div className="flex items-center gap-2"><ActorAvatar actorType="agent" name={preview.agent} size="sm" /><span>{preview.agent}</span></div>
              <p className="text-sm">{t(`googleAuthorization.mode.${preview.mode}`)}</p>
              <p className="text-sm">{t(`googleAuthorization.purpose.${preview.purpose}`)}</p>
              <details className="text-sm"><summary>{t('googleAuthorization.services')}</summary>
                <ul className="mt-2 space-y-2 break-all text-xs text-muted-foreground">{preview.services.map(scope => <li key={scope}>{scope}</li>)}</ul>
              </details>
              <GoogleDataDisclosure confirmed={confirmed} onConfirmedChange={setConfirmed} />
              <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={shared} onChange={e => setShared(e.target.checked)} />{t('googleAuthorization.shared')}</label>
              <p className="text-sm text-muted-foreground">{t('googleAuthorization.noSend')}</p>
              <Button disabled={busy || !confirmed || !shared} onClick={() => void confirm()}>{t('googleAuthorization.confirm')}</Button>
            </CardContent>
          </Card>}
    <Link to="/me/connections" className="text-sm underline">{t('myConnections.title')}</Link>
  </div>;
}
