import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { apiPost, useAuthStore, type LoginChallenge, type SessionTokens } from '@/stores/auth-store';
import { mfaApi, mfaErrorId } from '@/lib/mfa-api';
import { authenticatePasskey, passkeyAvailable, type RequestOptionsJSON } from '@/lib/passkey';
import { AlternativeEnrollment } from './AlternativeEnrollment';
import { MfaEnrollment } from './MfaEnrollment';

export function LoginMfa({ challenge, onComplete, onCancel }: { challenge: LoginChallenge; onComplete: () => void; onCancel: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [code, setCode] = useState('');
  const factors = challenge.factors ?? { totp: true, email: false, passkey: false };
  const [factor, setFactor] = useState<'totp' | 'email' | 'passkey'>(() =>
    challenge.last_used && factors[challenge.last_used] ? challenge.last_used : factors.totp ? 'totp' : factors.email ? 'email' : 'passkey');
  const [enrollFactor, setEnrollFactor] = useState<'totp' | 'email' | 'passkey'>('totp');
  const [recovery, setRecovery] = useState(false);
  const [sent, setSent] = useState(false);
  const [resendAt, setResendAt] = useState(0);
  const [clock, setClock] = useState(Date.now());
  useEffect(() => { const timer = window.setInterval(() => setClock(Date.now()), 1000); return () => window.clearInterval(timer); }, []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const session = useRef<SessionTokens | null>(null);
  const active = useRef(true);
  const attempt = useRef(useAuthStore.getState().authAttempt);
  useEffect(() => { active.current = true; return () => { active.current = false; session.current = null; }; }, []);
  const finish = async (tokens: SessionTokens) => {
    if (!active.current || attempt.current !== useAuthStore.getState().authAttempt) return;
    const established = await useAuthStore.getState().establishSession(tokens, attempt.current, () => active.current);
    if (established !== false && active.current && attempt.current === useAuthStore.getState().authAttempt) onComplete();
  };
  const verify = async (e: FormEvent) => {
    e.preventDefault();
    if (!('mfa_required' in challenge)) return;
    setBusy(true); setError('');
    try {
      let tokens: SessionTokens;
      if (factor === 'passkey' && !recovery) {
        const start = await apiPost<{ ceremony_id: string; publicKey: RequestOptionsJSON }>('/api/mfa/passkey/login/begin', { challenge_id: challenge.challenge_id });
        const credential = await authenticatePasskey(start.publicKey);
        tokens = await apiPost<SessionTokens>('/api/mfa/passkey/login/finish', { challenge_id: challenge.challenge_id, ceremony_id: start.ceremony_id, credential });
      } else if (factor === 'email' && !recovery) tokens = await mfaApi.emailVerify({ challenge_id: challenge.challenge_id, code });
      else tokens = await mfaApi.verify(challenge.challenge_id, code, recovery);
      if (active.current) await finish(tokens);
    } catch (error) { if (active.current) setError(t(mfaErrorId(error))); }
    finally { if (active.current) { setBusy(false); setCode(''); } }
  };
  const sendEmail = async () => {
    if (!('mfa_required' in challenge)) return;
    setBusy(true); setError('');
    try { await mfaApi.emailSend({ challenge_id: challenge.challenge_id }); if (active.current) { setSent(true); setResendAt(Date.now() + 60000); } }
    catch (error) { if (active.current) setError(t(mfaErrorId(error))); }
    finally { if (active.current) setBusy(false); }
  };
  if ('mfa_enrollment_required' in challenge) return <>
    <label className="block space-y-2">{t('mfa.choose')}<select className="w-full rounded-lg border bg-background p-2" value={enrollFactor} onChange={e => setEnrollFactor(e.target.value as typeof enrollFactor)}>
      <option value="totp">{t('mfa.factor.totp')}</option>
      <option value="email" disabled={!challenge.email_available}>{t('mfa.factor.email')}</option>
      <option value="passkey" disabled={!passkeyAvailable()}>{t('mfa.factor.passkey')}</option>
    </select></label>
    {!challenge.email_available && <p className="text-sm">{t('mfa.email.unavailable')}</p>}
    {!passkeyAvailable() && <p className="text-sm">{t('mfa.passkey.unavailable')}</p>}
    {enrollFactor !== 'totp' ? <AlternativeEnrollment factor={enrollFactor} enrollmentToken={challenge.enrollment_token} challengeRef={challenge.challenge_ref} onComplete={finish} onCancel={onCancel} /> : <>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <MfaEnrollment initial={{ enrollment_token: challenge.enrollment_token, challenge_ref: challenge.challenge_ref, proof_required: true }}
      begin={async input => ({ ...await mfaApi.httpBegin(input), enrollment_token: challenge.enrollment_token })}
      confirm={async input => {
        const result = await mfaApi.httpConfirm(input);
        if (active.current) session.current = { access_token: result.access_token, refresh_token: result.refresh_token };
        return { recovery_codes: result.recovery_codes };
      }}
      onSaved={async () => { if (session.current) { await finish(session.current); session.current = null; } }} onCancel={onCancel} />
    </>}
  </>;
  return <form onSubmit={verify} className="space-y-4">
    <h2 className="font-medium">{t('mfa.login.title')}</h2>
    <label className="block space-y-2">{t('mfa.choose')}<select disabled={busy} className="w-full rounded-lg border bg-background p-2" value={factor} onChange={e => { setFactor(e.target.value as typeof factor); setRecovery(false); setCode(''); setError(''); }}>
      {(['totp', 'email', 'passkey'] as const).filter(item => factors[item]).map(item => <option key={item} value={item}>{t(`mfa.factor.${item}`)}</option>)}
    </select></label>
    <p className="text-sm text-muted-foreground">{t(recovery ? 'mfa.login.hintRecovery' : factor === 'email' ? 'mfa.email.loginHint' : factor === 'passkey' ? 'mfa.passkey.loginHint' : 'mfa.login.hint')}</p>
    {factor === 'email' && !recovery && <>
      <Button type="button" variant="outline" disabled={busy || challenge.email_available === false || clock < resendAt} onClick={() => void sendEmail()}>{t(sent ? 'mfa.email.resend' : 'mfa.email.send')}{clock < resendAt ? ` (${Math.ceil((resendAt - clock) / 1000)})` : ''}</Button>
      {sent && <p role="status">{t('mfa.email.sent')}</p>}
      {challenge.email_available === false && <p role="status">{t('mfa.email.unavailable')}</p>}
    </>}
    {factor === 'passkey' && !recovery && !passkeyAvailable() && <p role="status">{t('mfa.passkey.unavailable')}</p>}
    {error && <p role="alert" className="text-destructive">{error}</p>}
    {(recovery || factor !== 'passkey') && <label className="block space-y-2">{t(recovery ? 'mfa.recovery.code' : factor === 'email' ? 'mfa.factor.email' : 'mfa.code')}
      <Input value={code} onChange={e => setCode(recovery ? e.target.value : e.target.value.replace(/\D/g, '').slice(0, 6))} required autoComplete={recovery ? 'off' : 'one-time-code'} inputMode={recovery ? 'text' : 'numeric'} />
    </label>}
    <Button type="submit" variant="brand" disabled={busy || (factor === 'passkey' && !recovery ? !passkeyAvailable() : (!recovery && code.length !== 6) || !code.trim())}>{t(busy ? 'mfa.working' : 'mfa.verify')}</Button>
    <Button type="button" variant="link" disabled={busy} onClick={() => { setRecovery(!recovery); setCode(''); }}>{t(recovery ? 'mfa.useTotp' : 'mfa.useRecovery')}</Button>
    <Button type="button" variant="outline" disabled={busy} onClick={onCancel}>{t('mfa.restart')}</Button>
  </form>;
}
