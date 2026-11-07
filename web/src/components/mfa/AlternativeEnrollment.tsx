import { DisabledFactorExplanation } from './DisabledFactorExplanation';
import { useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { apiPost, type SessionTokens } from '@/stores/auth-store';
import { mfaApi, mfaErrorId } from '@/lib/mfa-api';
import { createPasskey, passkeyAvailable, type CreationOptionsJSON } from '@/lib/passkey';
import { RecoveryCodes } from './MfaEnrollment';

export function AlternativeEnrollment({ factor, enrollmentToken, challengeRef, emailAvailable, emailUnavailableReason, onComplete, onCancel }: {
  factor: 'email' | 'passkey'; enrollmentToken: string; challengeRef?: string; emailAvailable?: boolean; emailUnavailableReason?: string | null;
  onComplete: (tokens: SessionTokens) => Promise<void>; onCancel: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [password, setPassword] = useState('');
  const [proof, setProof] = useState('');
  const [name, setName] = useState('');
  const [code, setCode] = useState('');
  const [sent, setSent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<(SessionTokens & { recovery_codes?: string[] }) | null>(null);
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setBusy(true); setError('');
    try {
      const input = { enrollment_token: enrollmentToken, password };
      if (factor === 'email') {
        if (!sent) { await mfaApi.emailSend(input); setSent(true); }
        else { const tokens = await mfaApi.emailVerify({ ...input, code }); if (tokens.recovery_codes?.length) setResult(tokens); else await onComplete(tokens); }
      } else {
        const start = await apiPost<{ ceremony_id: string; publicKey: CreationOptionsJSON }>('/api/mfa/passkey/register/begin', { ...input, proof_code: proof, name });
        const credential = await createPasskey(start.publicKey);
        const tokens = await apiPost<SessionTokens & { recovery_codes?: string[] }>('/api/mfa/passkey/register/finish', { enrollment_token: enrollmentToken, ceremony_id: start.ceremony_id, credential });
        if (tokens.recovery_codes?.length) setResult(tokens); else await onComplete(tokens);
      }
    } catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); setCode(''); }
  };
  if (result?.recovery_codes) return <RecoveryCodes codes={result.recovery_codes} onSaved={() => onComplete(result)} />;
  return <form onSubmit={submit} className="space-y-4">
    <h3>{t(`mfa.factor.${factor}`)}</h3>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <label className="block space-y-2">{t('mfa.password')}<Input type="password" autoComplete="current-password" maxLength={4096} value={password} onChange={e => setPassword(e.target.value)} required /></label>
    {factor === 'email' ? <>
      <p className="text-sm">{t('mfa.email.enrollHint')}</p>
      {emailAvailable === false && <DisabledFactorExplanation factor="email" reason={emailUnavailableReason} />}
      {sent && <label className="block space-y-2">{t('mfa.factor.email')}<Input autoComplete="one-time-code" inputMode="numeric" value={code} onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))} required pattern="[0-9]{6}" /></label>}
    </> : <>
      <p className="text-sm">{t('mfa.proof.hint')}</p>
      {challengeRef && <p className="break-all text-sm">{t('mfa.challengeRef')}: <code>{challengeRef}</code></p>}
      <label className="block space-y-2">{t('mfa.proof')}<Input autoComplete="off" value={proof} onChange={e => setProof(e.target.value)} required /></label>
      <label className="block space-y-2">{t('mfa.passkey.name')}<Input value={name} maxLength={80} onChange={e => setName(e.target.value)} required /></label>
      {!passkeyAvailable() && <DisabledFactorExplanation factor="passkey" />}
    </>}
    <Button type="submit" variant="brand" disabled={busy || (factor === 'email' && emailAvailable === false) || (factor === 'passkey' && !passkeyAvailable())}>{t(busy ? 'mfa.working' : factor === 'email' && !sent ? 'mfa.email.send' : 'mfa.confirm')}</Button>
    <Button type="button" variant="outline" disabled={busy} onClick={onCancel}>{t('mfa.cancel')}</Button>
  </form>;
}
