import { useEffect, useLayoutEffect, useState, type FormEvent } from 'react';
import { Link, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Button, Card, CardContent, CardHeader, CardTitle, Input } from '@/components/mds';
import { useEffectiveName } from '@/lib/product-identity';

export function invitePasswordError(password: string, confirm: string): 'policy' | 'mismatch' | null {
  const bytes = new TextEncoder().encode(password).length;
  if ([...password].length < 8 || bytes > 1024) return 'policy';
  return password !== confirm ? 'mismatch' : null;
}

export function InvitePage() {
  const [token, setToken] = useState<string | null>(null);
  useLayoutEffect(() => {
    // Capture once, then erase the bearer secret before paint or any request.
    // The updater preserves it when StrictMode replays this effect.
    const captured = window.location.hash.slice(1);
    window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search);
    setToken(current => current ?? captured);
  }, []);
  const intl = useIntl();
  const navigate = useNavigate();
  const brand = useEffectiveName();
  const t = (id: string) => intl.formatMessage({ id: `invite.${id}` });
  const [state, setState] = useState<'loading' | 'valid' | 'invalid' | 'error' | 'success'>('loading');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    // The invitation is a bearer secret. Do not let outbound navigation carry it.
    let meta = document.querySelector<HTMLMetaElement>('meta[name="referrer"]');
    const previous = meta?.content;
    const created = !meta;
    if (!meta) { meta = document.createElement('meta'); meta.name = 'referrer'; document.head.append(meta); }
    meta.content = 'no-referrer';
    return () => { if (created) meta?.remove(); else if (meta && previous !== undefined) meta.content = previous; };
  }, []);
  useEffect(() => {
    if (token === null) return;
    if (!token) { setState('invalid'); return; }
    const controller = new AbortController();
    setState('loading'); setPassword(''); setConfirm(''); setError('');
    void (async () => {
      try {
        const response = await fetch('/api/invite/check', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token }), signal: controller.signal, cache: 'no-store', credentials: 'omit', referrerPolicy: 'no-referrer' });
        if (!response.ok) throw new Error('request_failed');
        const data: unknown = await response.json();
        if (!controller.signal.aborted) {
          const result = data as { valid?: unknown; email_masked?: unknown };
          setEmail(typeof result?.email_masked === 'string' ? result.email_masked : '');
          setState(result?.valid === true ? 'valid' : 'invalid');
        }
      } catch { if (!controller.signal.aborted) setState('error'); }
    })();
    return () => controller.abort();
  }, [token]);
  useEffect(() => {
    if (state !== 'success') return;
    const timer = window.setTimeout(() => navigate('/login', { replace: true }), 1500);
    return () => window.clearTimeout(timer);
  }, [state, navigate]);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const validation = invitePasswordError(password, confirm);
    if (validation) { setError(validation); return; }
    setBusy(true); setError('');
    try {
      const response = await fetch('/api/invite/redeem', { method: 'POST', cache: 'no-store', credentials: 'omit', referrerPolicy: 'no-referrer', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token, password }) });
      const result = await response.json() as { valid?: unknown; code?: unknown };
      if (result?.code === 'password_policy') { setError('policy'); return; }
      if (!response.ok) throw new Error('request_failed');
      setPassword(''); setConfirm('');
      setState(result?.valid === true ? 'success' : 'invalid');
    } catch { setError('failed'); } finally { setBusy(false); }
  };
  return <main className="flex min-h-dvh items-center justify-center bg-background p-4">
    <Card className="w-full max-w-md">
      <CardHeader><p className="text-sm text-muted-foreground">{brand}</p><CardTitle>{t('title')}</CardTitle></CardHeader>
      <CardContent className="space-y-4">
        {state === 'loading' && <p role="status">{t('loading')}</p>}
        {state === 'invalid' && <p role="alert">{t('invalid')}</p>}
        {state === 'error' && <p role="alert">{t('failed')}</p>}
        {state === 'success' && <><p role="status">{t('success')}</p><Link className="underline" to="/login" replace>{t('login')}</Link></>}
        {state === 'valid' && <form onSubmit={event => void submit(event)} className="space-y-4">
          {email && <p className="text-sm text-muted-foreground">{email}</p>}
          <p className="text-sm">{t('intro')}</p>
          <div className="space-y-1"><label htmlFor="invite-password">{t('password')}</label><Input id="invite-password" type="password" autoComplete="new-password" value={password} onChange={e => setPassword(e.target.value)} disabled={busy} required /></div>
          <div className="space-y-1"><label htmlFor="invite-confirm">{t('confirm')}</label><Input id="invite-confirm" type="password" autoComplete="new-password" value={confirm} onChange={e => setConfirm(e.target.value)} disabled={busy} required /></div>
          {error && <p role="alert" className="text-sm text-destructive">{t(error)}</p>}
          <Button type="submit" variant="brand" disabled={busy || !password || !confirm}>{t('submit')}</Button>
        </form>}
        {(state === 'invalid' || state === 'error') && <Link className="underline" to="/login" replace>{t('login')}</Link>}
      </CardContent>
    </Card>
  </main>;
}
