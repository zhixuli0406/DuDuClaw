import { useEffect, useId, useRef, useState } from 'react';
import { KeyRound, Mail, Smartphone, UserRoundX } from 'lucide-react';
import { Button, Input, Spinner } from '@/components/mds';
import { authenticatePasskey, passkeyAvailable } from '@/lib/passkey';
import {
  myIsolationApi,
  type RestoreChallenge,
  type RestoreProof,
} from '@/lib/isolation-restore-api';
import { errorCode, needsAdminHelp } from '@/lib/restore-errors';
import { RestoreErrorNotice, useRestoreT } from './restore-ui';

type Phase =
  | { kind: 'loading' }
  | { kind: 'ready'; challenge: RestoreChallenge }
  | { kind: 'contact_admin'; reason: 'none' | 'changed' | 'credential' }
  | { kind: 'failed'; error: unknown };

/**
 * The owner proves the request with the strongest second factor they had
 * before the employee was tombstoned — the server's challenge decides which
 * one (passkey, authenticator code or email code). A factor added or
 * changed after the tombstone cannot prove it: the owner is sent to an Admin.
 */
export function FactorProof({
  employeeId,
  incarnation,
  forAdoption = false,
  submitLabel,
  canSubmit = true,
  onSubmit,
}: {
  employeeId: string;
  incarnation: number;
  forAdoption?: boolean;
  submitLabel: string;
  /** Other fields of the form are valid. */
  canSubmit?: boolean;
  onSubmit: (proof: RestoreProof) => Promise<void>;
}) {
  const t = useRestoreT();
  const [phase, setPhase] = useState<Phase>({ kind: 'loading' });
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const generation = useRef(0);
  const codeId = useId();
  const hintId = useId();

  const load = () => {
    const current = ++generation.current;
    setPhase({ kind: 'loading' });
    setError(null);
    setCode('');
    myIsolationApi
      .restoreChallenge(employeeId, incarnation, forAdoption)
      .then((challenge) => {
        if (current !== generation.current) return;
        if (challenge.factor === 'none' || challenge.factor === 'changed') {
          setPhase({ kind: 'contact_admin', reason: challenge.factor });
        } else setPhase({ kind: 'ready', challenge });
      })
      .catch((e) => {
        if (current !== generation.current) return;
        const c = errorCode(e);
        if (c === 'FACTOR_NOT_PREEXISTING') setPhase({ kind: 'contact_admin', reason: 'changed' });
        else if (c === 'CREDENTIAL_CHANGED') setPhase({ kind: 'contact_admin', reason: 'credential' });
        else setPhase({ kind: 'failed', error: e });
      });
  };

  useEffect(() => {
    load();
    return () => {
      generation.current += 1;
    };
  }, [employeeId, incarnation, forAdoption]); // eslint-disable-line react-hooks/exhaustive-deps

  const send = async (proof: RestoreProof) => {
    setBusy(true);
    setError(null);
    try {
      await onSubmit(proof);
    } catch (e) {
      const c = errorCode(e);
      if (needsAdminHelp(c)) setPhase({ kind: 'contact_admin', reason: c === 'CREDENTIAL_CHANGED' ? 'credential' : 'changed' });
      else setError(e);
      // A used or expired challenge is never retried silently.
      if (c === 'CHALLENGE_EXPIRED' || c === 'FACTOR_INVALID') setCode('');
    } finally {
      setBusy(false);
    }
  };

  const provePasskey = async (challenge: RestoreChallenge) => {
    if (!challenge.publicKey) {
      setError({ code: 'PASSKEY_UNAVAILABLE' });
      return;
    }
    setBusy(true);
    setError(null);
    let credential: unknown;
    try {
      credential = await authenticatePasskey(challenge.publicKey);
    } catch (e) {
      setBusy(false);
      setError(e instanceof Error && e.message === 'PASSKEY_UNAVAILABLE' ? { code: 'PASSKEY_UNAVAILABLE' } : { code: 'PASSKEY_CANCELLED' });
      return;
    }
    setBusy(false);
    await send({ factor: 'passkey', challenge_id: challenge.challenge_id, credential });
  };

  if (phase.kind === 'loading') {
    return (
      <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner aria-hidden="true" /> {t('restoreFlow.factor.loading')}
      </p>
    );
  }

  if (phase.kind === 'failed') {
    return (
      <div className="space-y-2">
        <RestoreErrorNotice error={phase.error} />
        <Button variant="outline" size="sm" onClick={load}>{t('restoreFlow.common.retry')}</Button>
      </div>
    );
  }

  if (phase.kind === 'contact_admin') {
    return (
      <div role="alert" className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2.5 text-sm">
        <UserRoundX className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
        <div className="space-y-1">
          <p className="font-medium">{t(`restoreFlow.factor.contactAdmin.${phase.reason}.title`)}</p>
          <p>{t(`restoreFlow.factor.contactAdmin.${phase.reason}.body`)}</p>
        </div>
      </div>
    );
  }

  const { challenge } = phase;
  const factor = challenge.factor as RestoreProof['factor'];
  const codeReady = /^\d{6}$/.test(code);
  const Icon = factor === 'passkey' ? KeyRound : factor === 'email' ? Mail : Smartphone;

  return (
    <div className="space-y-3">
      <div className="flex items-start gap-2 text-sm">
        <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        <div className="space-y-0.5">
          <p className="font-medium">{t(`restoreFlow.factor.${factor}.title`)}</p>
          <p id={hintId} className="text-muted-foreground">
            {factor === 'email'
              ? t('restoreFlow.factor.email.body', { email: challenge.email_hint ?? t('restoreFlow.factor.email.yourEmail') })
              : t(`restoreFlow.factor.${factor}.body`)}
          </p>
        </div>
      </div>

      {factor === 'passkey' ? (
        <>
          {!passkeyAvailable() && <p className="text-sm text-muted-foreground">{t('restoreFlow.factor.passkey.unsupported')}</p>}
          <Button variant="brand" disabled={busy || !canSubmit || !passkeyAvailable()} onClick={() => void provePasskey(challenge)}>
            {busy ? t('restoreFlow.common.sending') : t('restoreFlow.factor.passkey.action', { action: submitLabel })}
          </Button>
        </>
      ) : (
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (!busy && canSubmit && codeReady) void send({ factor, code, challenge_id: challenge.challenge_id });
          }}
        >
          <div className="grid gap-1.5">
            <label htmlFor={codeId} className="text-sm font-medium">{t(`restoreFlow.factor.${factor}.label`)}</label>
            <Input
              id={codeId}
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={6}
              value={code}
              onChange={(e) => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
              aria-describedby={hintId}
              className="w-40 font-mono tabular-nums"
            />
          </div>
          <Button type="submit" variant="brand" disabled={busy || !canSubmit || !codeReady}>
            {busy ? t('restoreFlow.common.sending') : submitLabel}
          </Button>
          {factor === 'email' && (
            <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={load}>
              {t('restoreFlow.factor.email.resend')}
            </Button>
          )}
        </form>
      )}

      <RestoreErrorNotice error={error} />
    </div>
  );
}
