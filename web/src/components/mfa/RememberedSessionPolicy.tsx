import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { sessionAdminApi } from '@/lib/session-admin-api';
import { mfaErrorId } from '@/lib/mfa-api';
import { useAuthStore } from '@/stores/auth-store';

export function RememberedSessionPolicy() {
  const user = useAuthStore(s => s.user);
  return user?.role === 'admin' ? <PolicyForm key={user.id} /> : null;
}

function validDays(days: number) {
  return Number.isInteger(days) && days >= 1 && days <= 365;
}

function PolicyForm() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [saved, setSaved] = useState<number | null>(null);
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    setError('');
    void sessionAdminApi.policy().then(result => {
      if (!validDays(result?.days)) throw new Error('Invalid policy');
      if (active) { setSaved(result.days); setValue(String(result.days)); }
    }).catch(error => { if (active) setError(t(mfaErrorId(error))); });
    return () => { active = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attempt]);
  const days = Number(value);
  const valid = value.trim() !== '' && validDays(days);
  const save = async () => {
    if (busy || saved === null || !valid || useAuthStore.getState().user?.role !== 'admin') return;
    setBusy(true); setError(''); setNotice(false);
    try {
      const result = await sessionAdminApi.updatePolicy(days);
      if (!validDays(result?.days)) throw new Error('Invalid policy');
      setSaved(result.days); setValue(String(result.days)); setNotice(true);
    } catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); }
  };
  return <SettingsSection title={t('sessions.policy.title')} description={t('sessions.policy.description')}>
    <SettingsCard><form onSubmit={event => { event.preventDefault(); void save(); }}>
      <SettingsRow label={<label htmlFor="remember-days">{t('sessions.policy.days')}</label>} description={t('sessions.policy.range')} tier="select">
        <Input id="remember-days" type="number" min={1} max={365} step={1} value={value}
          disabled={busy || saved === null} aria-invalid={saved !== null && !valid}
          onChange={event => { setValue(event.target.value); setNotice(false); }} />
      </SettingsRow>
      <div className="space-y-3 px-4 pb-4">
        {saved === null && !error && <p role="status">{t('mfa.working')}</p>}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        {notice && <p role="status" className="text-sm">{t('sessions.policy.updated')}</p>}
        {saved === null && error
          ? <Button type="button" variant="outline" onClick={() => setAttempt(attempt + 1)}>{t('mfa.refresh')}</Button>
          : <Button type="submit" disabled={busy || saved === null || !valid || days === saved}>{t('common.save')}</Button>}
      </div>
    </form></SettingsCard>
  </SettingsSection>;
}
