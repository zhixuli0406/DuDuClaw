import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { QrCode } from '@/components/shared/QrCode';
import { mfaErrorId, type Enrollment, type EnrollmentInput, type RecoveryResult } from '@/lib/mfa-api';

export function RecoveryCodes({ codes, onSaved }: { codes: string[]; onSaved: () => void | Promise<void> }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState('');
  const [copyState, setCopyState] = useState('');
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(codes.join('\n'));
      setCopyState(t('mfa.copied'));
    } catch { setCopyState(t('mfa.error.copy')); }
  };
  const download = () => {
    const url = URL.createObjectURL(new Blob([codes.join('\n') + '\n'], { type: 'text/plain' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'duduclaw-recovery-codes.txt';
    link.click();
    URL.revokeObjectURL(url);
  };
  return <div className="space-y-4">
    <p>{t('mfa.recovery.once')}</p>
    <pre className="overflow-x-auto rounded-lg border p-3 text-sm" aria-label={t('mfa.recovery.title')}>{codes.join('\n')}</pre>
    <div className="flex flex-wrap gap-2">
      <Button variant="outline" onClick={() => void copy()}>{t('mfa.copy')}</Button>
      <Button variant="outline" onClick={download}>{t('mfa.download')}</Button>
    </div>
    {copyState && <p role="status">{copyState}</p>}
    <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={saved} onChange={e => setSaved(e.target.checked)} />{t('mfa.saved')}</label>
    {saveError && <p role="alert" className="text-destructive">{saveError}</p>}
    <Button variant="brand" disabled={!saved || busy} onClick={() => {
      setBusy(true); setSaveError('');
      void Promise.resolve().then(onSaved).catch(error => setSaveError(t(mfaErrorId(error)))).finally(() => setBusy(false));
    }}>{t('mfa.continue')}</Button>
  </div>;
}

export function MfaEnrollment({ initial, begin, confirm, onSaved, onCancel }: {
  initial?: Enrollment;
  begin: (input: EnrollmentInput) => Promise<Enrollment>;
  confirm: (input: EnrollmentInput) => Promise<RecoveryResult>;
  onSaved: () => void | Promise<void>;
  onCancel: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [enrollment, setEnrollment] = useState<Enrollment | undefined>(initial);
  const [password, setPassword] = useState('');
  const [proof, setProof] = useState('');
  const [code, setCode] = useState('');
  const [codes, setCodes] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  // All sensitive values are component-local. No store, storage, URL or log receives them.
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true); setError('');
    try {
      const input = { password, enrollment_token: enrollment?.enrollment_token, proof_code: proof };
      if (enrollment?.secret) {
        const result = await confirm({ ...input, code });
        if (!mounted.current) return;
        setCodes(result.recovery_codes);
        setEnrollment(undefined); setPassword(''); setProof(''); setCode('');
      } else {
        const result = await begin(input);
        if (mounted.current) setEnrollment(result);
      }
    } catch (error) { if (mounted.current) setError(t(mfaErrorId(error))); }
    finally { if (mounted.current) setBusy(false); }
  };
  if (codes) return <RecoveryCodes codes={codes} onSaved={async () => { await onSaved(); if (mounted.current) setCodes(null); }} />;
  return <form onSubmit={submit} className="space-y-4">
    <h3 className="font-medium">{t('mfa.enroll')}</h3>
    <p className="text-sm text-muted-foreground">{t('mfa.enroll.hint')}</p>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {enrollment?.secret ? <>
      {enrollment.otpauth_uri && <div role="img" aria-label={t('mfa.qr')}><QrCode value={enrollment.otpauth_uri} /></div>}
      <p className="text-sm">{t('mfa.secret')}</p>
      <code className="block break-all select-all">{enrollment.secret}</code>
      <label className="block space-y-2">{t('mfa.code')}<Input autoComplete="one-time-code" inputMode="numeric" value={code} onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))} required pattern="[0-9]{6}" /></label>
    </> : <>
      <label className="block space-y-2">{t('mfa.password')}<Input type="password" maxLength={4096} autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} required /></label>
      {enrollment?.enrollment_token && <>
        <p className="text-sm">{t('mfa.proof.hint')}</p>
        {enrollment.challenge_ref && <p className="break-all text-sm">{t('mfa.challengeRef')}: <code>{enrollment.challenge_ref}</code></p>}
        <label className="block space-y-2">{t('mfa.proof')}<Input autoComplete="off" value={proof} onChange={e => setProof(e.target.value)} required /></label>
      </>}
    </>}
    <div className="flex flex-wrap gap-2">
      <Button type="submit" variant="brand" disabled={busy}>{busy ? t('mfa.working') : t(enrollment?.secret ? 'mfa.confirm' : 'mfa.enroll.begin')}</Button>
      <Button type="button" variant="outline" disabled={busy} onClick={onCancel}>{t('mfa.cancel')}</Button>
    </div>
  </form>;
}
