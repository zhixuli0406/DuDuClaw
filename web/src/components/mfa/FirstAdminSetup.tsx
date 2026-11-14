import { useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { ApiError } from '@/stores/auth-store';
import { mfaApi, mfaErrorId } from '@/lib/mfa-api';

/** The host command that prints the one-time setup code (never shown in logs). */
const SETUP_CODE_COMMAND = 'duduclaw ops users mfa-setup-code';

/**
 * RM-135: the first administrator of a new installation confirms access to
 * the host once, before the ordinary first-time two-factor setup, with the
 * one-time setup code the host command prints. There is no code-free path.
 * The code stays in this component only; it never reaches a store,
 * storage, URL or log.
 */
export function FirstAdminSetup({ enrollmentToken, onConfirmed }: { enrollmentToken: string; onConfirmed: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const setupCode = code.trim();
    if (!setupCode) return;
    setBusy(true); setError('');
    try {
      await mfaApi.bootstrap(enrollmentToken, setupCode);
      onConfirmed();
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 401) setError(t('mfa.bootstrap.refused'));
      else setError(t(mfaErrorId(failure)));
    } finally { setBusy(false); setCode(''); }
  };
  return <section className="space-y-3 rounded-lg border p-3">
    <h3 className="font-medium">{t('mfa.bootstrap.title')}</h3>
    <p className="text-sm text-muted-foreground">{t('mfa.bootstrap.hint')}</p>
    <code className="block rounded bg-muted p-2 text-sm">{SETUP_CODE_COMMAND}</code>
    <p className="text-xs text-muted-foreground">{t('mfa.bootstrap.docker')}</p>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <form onSubmit={e => void submit(e)} className="space-y-2">
      <label className="block space-y-2">{t('mfa.bootstrap.code')}
        <Input autoComplete="off" spellCheck={false} maxLength={64} value={code} onChange={e => setCode(e.target.value)} required />
      </label>
      <Button type="submit" disabled={busy || !code.trim()}>{t('mfa.bootstrap.useCode')}</Button>
    </form>
  </section>;
}
