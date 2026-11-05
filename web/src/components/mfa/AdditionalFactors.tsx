import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore, type SessionTokens } from '@/stores/auth-store';
import { accountRequest, mfaApi, mfaErrorId, type MfaStatus } from '@/lib/mfa-api';
import { createPasskey, passkeyAvailable, type CreationOptionsJSON } from '@/lib/passkey';
import { RecoveryCodes } from './MfaEnrollment';

type Passkey = { id: string; name: string; created_at: number };
export function AdditionalFactors({ status, onChange }: { status: MfaStatus; onChange: () => Promise<void> }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [password, setPassword] = useState('');
  const [name, setName] = useState('');
  const [code, setCode] = useState('');
  const [enrollment, setEnrollment] = useState('');
  const [passkeys, setPasskeys] = useState<Passkey[]>([]);
  const [available, setAvailable] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [codes, setCodes] = useState<string[] | null>(null);
  const [passkeyEnrollment, setPasskeyEnrollment] = useState<{ enrollment_token: string; challenge_ref?: string } | null>(null);
  const [proof, setProof] = useState('');
  const active = useRef(true);
  const requestAttempt = useRef(0);
  const requestRefresh = useRef<string | null>(null);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const load = async () => {
    const data = await accountRequest<{ passkeys: Passkey[]; available: boolean }>('/api/mfa/passkey/list', {});
    setPasskeys(data.passkeys); setAvailable(data.available);
  };
  useEffect(() => { void load().catch(error => setError(t(mfaErrorId(error)))); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const run = async (action: () => Promise<void | boolean>) => {
    requestAttempt.current = useAuthStore.getState().authAttempt;
    requestRefresh.current = useAuthStore.getState().refreshToken;
    let keepPassword = false;
    setBusy(true); setError('');
    try { keepPassword = await action() === true; if (active.current && requestAttempt.current === useAuthStore.getState().authAttempt) await onChange(); }
    catch (error) { setError(t(mfaErrorId(error))); }
    finally { if (active.current) { setBusy(false); if (!keepPassword) setPassword(''); setCode(''); } }
  };
  const replace = (result: SessionTokens) => {
    if (active.current && requestAttempt.current === useAuthStore.getState().authAttempt)
      useAuthStore.getState().replaceSessionTokens(result, requestRefresh.current);
  };
  const reconnect = async (result: SessionTokens) => {
    const established = await useAuthStore.getState().establishSession(result, requestAttempt.current, () => active.current);
    if (!established) return;
    useConnectionStore.getState().disconnect();
    await useConnectionStore.getState().connectWithAuth(() => useAuthStore.getState().jwt ?? undefined);
  };
  const removeDisabled = status.enforced && !status.factors?.totp && Number(!!status.factors?.email) + passkeys.length <= 1;
  if (codes) return <RecoveryCodes codes={codes} onSaved={() => setCodes(null)} />;
  return <div className="space-y-4 border-t pt-4">
    <h3 className="font-medium">{t('mfa.methods')}</h3>
    <p className="text-sm text-muted-foreground">{t('mfa.reauthHint')}</p>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <label className="block space-y-2">{t('mfa.factorPassword')}<Input type="password" autoComplete="current-password" maxLength={4096} value={password} onChange={e => setPassword(e.target.value)} /></label>
    <div className="space-y-3">
      <h4>{t('mfa.factor.email')}</h4>
      {!status.email_available && <p role="status" className="text-sm">{t('mfa.email.unavailable')}</p>}
      {status.factors?.email ? <Button variant="outline" disabled={busy || !password || removeDisabled} onClick={() => void run(async () => replace(await mfaApi.emailRemove(password)))}>{t('mfa.email.remove')}</Button> : <>
        <p className="text-sm">{t('mfa.email.enrollHint')}</p>
        {enrollment && <label className="block space-y-2">{t('mfa.factor.email')}<Input autoComplete="one-time-code" inputMode="numeric" value={code} onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))} /></label>}
        <Button variant="outline" disabled={busy || !status.email_available || (enrollment ? code.length !== 6 : !password)} onClick={() => void run(async () => {
          if (!enrollment) setEnrollment((await mfaApi.emailBegin(password)).enrollment_token);
          else { const result = await mfaApi.emailConfirm(enrollment, code); replace(result); if (result.recovery_codes?.length) setCodes(result.recovery_codes); setEnrollment(''); }
        })}>{t(enrollment ? 'mfa.confirm' : 'mfa.email.add')}</Button>
        {enrollment && <p role="status" className="text-sm">{t('mfa.email.sent')}</p>}
      </>}
    </div>
    <div className="space-y-3">
      <h4>{t('mfa.factor.passkey')}</h4>
      {(!available || !passkeyAvailable()) && <p role="status" className="text-sm">{t('mfa.passkey.unavailable')}</p>}
      <label className="block space-y-2">{t('mfa.passkey.name')}<Input maxLength={80} value={name} onChange={e => setName(e.target.value)} /></label>
      {passkeyEnrollment && <>
        <p className="text-sm">{t('mfa.proof.hint')}</p>
        {passkeyEnrollment.challenge_ref && <p className="break-all text-sm">{t('mfa.challengeRef')}: <code>{passkeyEnrollment.challenge_ref}</code></p>}
        <label className="block space-y-2">{t('mfa.proof')}<Input autoComplete="off" value={proof} onChange={e => setProof(e.target.value)} /></label>
      </>}
      <Button variant="outline" disabled={busy || !password || !name.trim() || !available || !passkeyAvailable() || (!!passkeyEnrollment && !proof)} onClick={() => void run(async () => {
        const start = await accountRequest<{ ceremony_id?: string; enrollment_token: string; challenge_ref?: string; proof_required?: boolean; publicKey?: CreationOptionsJSON }>('/api/mfa/passkey/register/begin', { password, name,
          ...(passkeyEnrollment ? { enrollment_token: passkeyEnrollment.enrollment_token, proof_code: proof } : {}) });
        if (!active.current || requestAttempt.current !== useAuthStore.getState().authAttempt) return;
        if (start.proof_required && !start.publicKey) { setPasskeyEnrollment(start); return true; }
        if (!start.publicKey || !start.ceremony_id) throw new Error('Invalid passkey response');
        const credential = await createPasskey(start.publicKey);
        if (!active.current || requestAttempt.current !== useAuthStore.getState().authAttempt) return;
        const result = await accountRequest<SessionTokens & { recovery_codes?: string[] }>('/api/mfa/passkey/register/finish', { ceremony_id: start.ceremony_id, enrollment_token: start.enrollment_token, credential });
        await reconnect(result);
        if (result.recovery_codes?.length) setCodes(result.recovery_codes);
        setName(''); setPasskeyEnrollment(null); setProof(''); await load();
      })}>{t('mfa.passkey.add')}</Button>
      <ul className="space-y-3">{passkeys.map(key => <li key={key.id} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border p-3">
        <span>{key.name}<span className="block text-xs text-muted-foreground">{intl.formatDate(new Date(key.created_at * 1000))}</span></span>
        <Button variant="outline" disabled={busy || !password || removeDisabled} onClick={() => void run(async () => {
          const result = await accountRequest<SessionTokens>('/api/mfa/passkey/delete', { password, id: key.id });
          await reconnect(result); await load();
        })}>{t('mfa.passkey.remove')}</Button>
      </li>)}</ul>
      {removeDisabled && <p className="text-sm">{t('mfa.lastFactor')}</p>}
    </div>
  </div>;
}
