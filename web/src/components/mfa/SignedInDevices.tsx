import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, SettingsCard, SettingsSection } from '@/components/mds';
import { useAuthStore } from '@/stores/auth-store';
import { accountRequest, mfaErrorId } from '@/lib/mfa-api';
interface Device { id: string; label: string; created_at: number; last_used_at: number; expires_at: number; ip?: string; current: boolean }
export function SignedInDevices() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const jwt = useAuthStore(s => s.jwt);
  const load = async () => {
    const expected = useAuthStore.getState().jwt;
    const result = await accountRequest<{ devices: Device[] }>('/api/session/devices');
    if (expected === useAuthStore.getState().jwt) setDevices(result.devices);
  };
  useEffect(() => { if (jwt) void load().catch(error => setError(t(mfaErrorId(error)))); }, [jwt]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!jwt) return null;
  const revoke = async (device?: Device) => {
    const attempt = useAuthStore.getState().authAttempt;
    setBusy(true); setError('');
    try {
      await accountRequest('/api/session/revoke', device ? { id: device.id } : { all: true });
      if (attempt !== useAuthStore.getState().authAttempt) return;
      if (!device || device.current) useAuthStore.getState().logout(); else await load();
    } catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); }
  };
  return <SettingsSection title={t('mfa.devices.title')} description={t('mfa.devices.hint')}><SettingsCard><div className="space-y-4 p-4">
    {error && <p role="alert" className="text-destructive">{error}</p>}
    {devices === null ? <p role="status">{t('mfa.working')}</p> : devices.length === 0 ? <p>{t('mfa.devices.empty')}</p> : <ul className="space-y-3">{devices.map(device => <li key={device.id} className="flex flex-wrap items-center justify-between gap-3 rounded-lg border p-3">
      <div><p>{device.label} {device.current && <span className="text-sm">({t('mfa.devices.current')})</span>}</p>
        <p className="text-xs text-muted-foreground">{t('mfa.devices.created')}: {intl.formatDate(new Date(device.created_at * 1000))}</p>
        <p className="text-xs text-muted-foreground">{t('mfa.devices.lastUsed')}: {intl.formatDate(new Date(device.last_used_at * 1000))} {intl.formatTime(new Date(device.last_used_at * 1000))}</p>
        {device.ip && <p className="text-xs text-muted-foreground">{device.ip}</p>}
      </div><Button variant="outline" disabled={busy} onClick={() => void revoke(device)}>{t('mfa.devices.revoke')}</Button>
    </li>)}</ul>}
    <Button variant="destructive" disabled={busy} onClick={() => void revoke()}>{t('mfa.devices.revokeAll')}</Button>
  </div></SettingsCard></SettingsSection>;
}
