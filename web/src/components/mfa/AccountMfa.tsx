import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input, SettingsSection, SettingsCard } from '@/components/mds';
import { mfaApi, mfaErrorId, type EnrollmentInput } from '@/lib/mfa-api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { MfaEnrollment, RecoveryCodes } from './MfaEnrollment';

export function AccountMfa() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const edition = useSystemStore(s => s.status?.edition_profile);
  const [status, setStatus] = useState<{ enabled: boolean; enforced: boolean } | null>(null);
  const [error, setError] = useState('');
  const [enrolling, setEnrolling] = useState(false);
  const [code, setCode] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [codes, setCodes] = useState<string[] | null>(null);
  useEffect(() => {
    let active = true;
    void mfaApi.status().then(value => { if (active) setStatus(value); })
      .catch(error => { if (active) setError(t(mfaErrorId(error))); });
    return () => { active = false; };
    // Reloading the locale must not restart an enrollment.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Unknown edition fails closed; Personal exposes only an existing enrollment.
  if (!edition || (edition === 'personal' && !status?.enabled)) return null;
  const confirmEnrollment = async (input: EnrollmentInput) => {
    const previousRefreshToken = useAuthStore.getState().refreshToken;
    const result = await mfaApi.confirm(input);
    useAuthStore.getState().replaceSessionTokens(result, previousRefreshToken);
    return result;
  };
  const finishEnrollment = () => {
    setStatus(previous => previous && { ...previous, enabled: true });
    setEnrolling(false);
  };
  const change = async (disable: boolean) => {
    setBusy(true); setError('');
    try {
      if (disable) {
        const previousRefreshToken = useAuthStore.getState().refreshToken;
        const result = await mfaApi.disable(code, password);
        useAuthStore.getState().replaceSessionTokens(result, previousRefreshToken);
        setStatus(previous => previous && { ...previous, enabled: false });
      }
      else { const result = await mfaApi.regenerate(code); setCodes(result.recovery_codes); }
      setCode('');
    } catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); setPassword(''); }
  };
  return <SettingsSection title={t('mfa.title')} description={t('mfa.account.hint')}>
    <SettingsCard><div className="space-y-4 p-4">
      {error && <p role="alert" className="text-destructive">{error}</p>}
      {!status && !error && <p role="status">{t('mfa.working')}</p>}
      {codes ? <RecoveryCodes codes={codes} onSaved={() => setCodes(null)} /> : enrolling ?
        <MfaEnrollment begin={mfaApi.begin} confirm={confirmEnrollment} onSaved={finishEnrollment} onCancel={() => setEnrolling(false)} /> : status && <>
          <p>{t(status.enabled ? 'mfa.enabled' : 'mfa.notEnrolled')}</p>
          {status.enforced && <p className="text-sm">{t('mfa.enforced')}</p>}
          {status.enabled ? <>
            <label className="block space-y-2">{t('mfa.freshFactor')}<Input autoComplete="off" value={code} onChange={e => setCode(e.target.value)} /></label>
            {!status.enforced && <label className="block space-y-2">{t('mfa.password')}<Input type="password" maxLength={4096} autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} /></label>}
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" disabled={busy || !code.trim()} onClick={() => void change(false)}>{t('mfa.regenerate')}</Button>
              <Button variant="destructive" disabled={busy || status.enforced || !code.trim() || !password} onClick={() => void change(true)}>{t('mfa.disable')}</Button>
            </div>
          </> : <Button variant="brand" onClick={() => setEnrolling(true)}>{t('mfa.enroll')}</Button>}
        </>}
    </div></SettingsCard>
  </SettingsSection>;
}
