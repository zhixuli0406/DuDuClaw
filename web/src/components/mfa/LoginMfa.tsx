import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { useAuthStore, type LoginChallenge, type SessionTokens } from '@/stores/auth-store';
import { mfaApi, mfaErrorId } from '@/lib/mfa-api';
import { MfaEnrollment } from './MfaEnrollment';

export function LoginMfa({ challenge, onComplete, onCancel }: { challenge: LoginChallenge; onComplete: () => void; onCancel: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [code, setCode] = useState('');
  const [recovery, setRecovery] = useState(false);
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
      const tokens = await mfaApi.verify(challenge.challenge_id, code, recovery);
      if (active.current) await finish(tokens);
    } catch (error) { if (active.current) setError(t(mfaErrorId(error))); }
    finally { if (active.current) { setBusy(false); setCode(''); } }
  };
  if ('mfa_enrollment_required' in challenge) return <>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <MfaEnrollment initial={{ enrollment_token: challenge.enrollment_token, challenge_ref: challenge.challenge_ref, proof_required: true }}
      begin={async input => ({ ...await mfaApi.httpBegin(input), enrollment_token: challenge.enrollment_token })}
      confirm={async input => {
        const result = await mfaApi.httpConfirm(input);
        if (active.current) session.current = { access_token: result.access_token, refresh_token: result.refresh_token };
        return { recovery_codes: result.recovery_codes };
      }}
      onSaved={async () => { if (session.current) { await finish(session.current); session.current = null; } }} onCancel={onCancel} />
  </>;
  return <form onSubmit={verify} className="space-y-4">
    <h2 className="font-medium">{t('mfa.login.title')}</h2>
    <p className="text-sm text-muted-foreground">{t('mfa.login.hint')}</p>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <label className="block space-y-2">{t(recovery ? 'mfa.recovery.code' : 'mfa.code')}
      <Input value={code} onChange={e => setCode(recovery ? e.target.value : e.target.value.replace(/\D/g, '').slice(0, 6))} required autoComplete={recovery ? 'off' : 'one-time-code'} inputMode={recovery ? 'text' : 'numeric'} />
    </label>
    <Button type="submit" variant="brand" disabled={busy || (!recovery && code.length !== 6) || !code.trim()}>{t(busy ? 'mfa.working' : 'mfa.verify')}</Button>
    <Button type="button" variant="link" disabled={busy} onClick={() => { setRecovery(!recovery); setCode(''); }}>{t(recovery ? 'mfa.useTotp' : 'mfa.useRecovery')}</Button>
    <Button type="button" variant="outline" disabled={busy} onClick={onCancel}>{t('mfa.restart')}</Button>
  </form>;
}
