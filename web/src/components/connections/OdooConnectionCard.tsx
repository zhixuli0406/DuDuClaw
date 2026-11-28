import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type MyOdooStatus } from '@/lib/api';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { settingsRefusal } from '@/lib/settings-refusal';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Input, Skeleton, useErrorMessage } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';

const confirmServerRefusals = new Set([
  '請先重新整理連線頁面，確認 Odoo 伺服器位址和資料庫後再連線或重新驗證。',
  '連線資料已變更，請重新整理頁面，確認伺服器位址和資料庫後再試。',
]);

export function OdooConnectionCard() {
  const intl = useIntl();
  useCompanyTimeZone();
  const t = (key: string, values?: Record<string, string>) => intl.formatMessage({ id: `myOdoo.${key}` }, values);
  const errorText = useErrorMessage();
  const [status, setStatus] = useState<MyOdooStatus | null>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [login, setLogin] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [reconnect, setReconnect] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const lock = useRef(false);
  const id = useId();

  const load = useCallback(async () => {
    setReady(false);
    const result = await api.myOdoo.status();
    setStatus(result);
    setReady(true);
  }, []);
  useEffect(() => {
    let active = true;
    void api.myOdoo.status().then(result => {
      if (active) { setStatus(result); setReady(true); }
    }).catch(err => { if (active) setError(settingsRefusal(err, errorText(err))); });
    return () => { active = false; };
  }, [errorText]);

  const run = async (action: 'connect' | 'test' | 'disconnect' | 'reload') => {
    if (lock.current || (action !== 'reload' && (!ready || !status?.member_credentials_enabled))) return;
    if (action === 'connect' && (!login.trim() || !apiKey)) return;
    lock.current = true;
    setBusy(true);
    setError('');
    const key = apiKey;
    setApiKey('');
    try {
      if (action === 'reload') await load();
      else if (action === 'disconnect') {
        await api.myOdoo.disconnect();
        setConfirm(false);
        setReconnect(false);
        // A fresh status also records the server before the next connection.
        setStatus(null);
        await load();
      } else {
        setStatus(await (action === 'connect' ? api.myOdoo.connect({ login, api_key: key }) : api.myOdoo.test()));
        setReconnect(false);
      }
    } catch (err) {
      const message = settingsRefusal(err, errorText(err));
      setError(message);
      if (confirmServerRefusals.has(message)) {
        try { await load(); setError(`${message} ${t('retryConnection')}`); }
        catch (refreshError) { setError(settingsRefusal(refreshError, errorText(refreshError))); }
      }
    } finally {
      setApiKey('');
      lock.current = false;
      setBusy(false);
    }
  };
  const enabled = status?.member_credentials_enabled === true;
  const showForm = enabled && (!status.connected || reconnect);
  const badge = status && (!enabled ? 'admin' : status.connected && !reconnect ? status.state : null);
  return <Card>
    <CardHeader className="flex-row items-center justify-between gap-3">
      <CardTitle>{t('title')}</CardTitle>
      {badge && <Badge variant="secondary">{t(`badge.${badge}`)}</Badge>}
    </CardHeader>
    <CardContent className="space-y-3">
      {!status && !error && <Skeleton className="h-20 w-full" />}
      {!status && <Button disabled>{t('connect')}</Button>}
      {status && !enabled && <p className="text-sm text-muted-foreground">{t('adminNote')}</p>}
      {showForm && <form className="space-y-3" onSubmit={event => { event.preventDefault(); void run('connect'); }}>
        <p className="text-sm text-muted-foreground">{t('intro')}</p>
        <div className="space-y-1.5"><label htmlFor={`${id}-login`} className="text-sm">{t('login')}</label>
          <Input id={`${id}-login`} value={login} disabled={busy || !ready} onChange={event => setLogin(event.target.value)} autoComplete="off" /></div>
        <div className="space-y-1.5"><label htmlFor={`${id}-key`} className="text-sm">{t('apiKey')}</label>
          <Input id={`${id}-key`} type="password" autoComplete="off" value={apiKey} disabled={busy || !ready} onChange={event => setApiKey(event.target.value)} /></div>
        <Button type="submit" disabled={busy || !ready || !login.trim() || !apiKey}>{t('connect')}</Button>
      </form>}
      {enabled && status.server && (showForm || error) && <p className="break-all text-xs text-muted-foreground">{t('serverNote', { server: status.server })}</p>}
      {enabled && status.connected && !reconnect && <>
        {status.state === 'ok' && <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-2 text-sm">
          {([
            ['login', status.login], ['odooName', status.odoo_name], ['server', status.server],
            ['verifiedAt', formatDateTime(status.verified_at, { locale: intl.locale })],
            ['lastUsedAt', formatDateTime(status.last_used_at, { locale: intl.locale })],
          ] as const).map(([label, value]) => <div key={label} className="contents"><dt className="text-muted-foreground">{t(label)}</dt><dd className="break-all">{value ?? '—'}</dd></div>)}
        </dl>}
        {status.state === 'needs_reverify' && <p className="text-sm text-warning">{t('needsReverify', { server: status.server ?? '—' })}</p>}
        {status.state === 'invalid' && <p className="text-sm text-destructive">{t('invalid')}</p>}
        <div className="flex flex-wrap gap-2">
          {status.state === 'invalid' ? <Button disabled={busy || !ready} onClick={() => { setLogin(status.login ?? ''); setReconnect(true); setError(''); }}>{t('reconnect')}</Button>
            : <Button variant="outline" disabled={busy || !ready} onClick={() => void run('test')}>{t('test')}</Button>}
          <Button variant="outline" disabled={busy || !ready} onClick={() => setConfirm(true)}>{t('disconnect')}</Button>
        </div>
      </>}
      {error && !confirm && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {!ready && error && <Button variant="outline" disabled={busy} onClick={() => void run('reload')}>{t('reload')}</Button>}
      <ConfirmDialog open={confirm} onClose={() => { if (!lock.current) setConfirm(false); }} onConfirm={() => void run('disconnect')}
        title={t('disconnectTitle')} message={t('disconnectNote')} confirmLabel={t('disconnect')} busy={busy}>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      </ConfirmDialog>
    </CardContent>
  </Card>;
}
