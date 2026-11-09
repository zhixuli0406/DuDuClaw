import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ShieldCheck } from 'lucide-react';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Spinner,
} from '@/components/mds';
import { stepUpApi, type StepUpToken } from '@/lib/step-up';
import type { BreakGlassPurpose } from '@/lib/break-glass';

export type StepUpPurpose = 'admin' | BreakGlassPurpose;

type Phase = 'loading' | 'valid' | 'verify' | 'no_mfa' | 'load_failed';

/**
 * 需要再驗證身分 — password + authenticator code (five minutes, this session).
 *
 * `admin`: protected-mode Admin actions (restore, purge, retire, re-grant).
 * `read` / `write` / `decide`: P1 break-glass on someone else's Personal
 * employee. A still-valid proof is never used silently for break-glass: the
 * Admin confirms each time, because every such access is recorded.
 */
export function StepUpDialog({
  open,
  purpose,
  onDone,
}: {
  open: boolean;
  purpose: StepUpPurpose;
  onDone: (token: StepUpToken | null) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [phase, setPhase] = useState<Phase>('loading');
  const [token, setToken] = useState<StepUpToken | null>(null);
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const generation = useRef(0);
  const passwordId = useId();
  const codeId = useId();
  const errorId = useId();

  useEffect(() => {
    if (!open) return;
    const current = ++generation.current;
    setPhase('loading');
    setToken(null);
    setPassword('');
    setCode('');
    setFailed(false);
    stepUpApi
      .status()
      .then((status) => {
        if (current !== generation.current) return;
        if (!status.mfa_enrolled) setPhase('no_mfa');
        else if (status.step_up_valid && status.csrf_token) {
          setToken(status.csrf_token);
          setPhase('valid');
        } else setPhase('verify');
      })
      .catch(() => {
        if (current === generation.current) setPhase('load_failed');
      });
    return () => {
      generation.current += 1;
    };
  }, [open]);

  const close = (result: StepUpToken | null) => {
    generation.current += 1;
    setPassword('');
    setCode('');
    onDone(result);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || !password || code.length !== 6) return;
    setBusy(true);
    setFailed(false);
    const current = generation.current;
    try {
      await stepUpApi.verify(password, code);
      const status = await stepUpApi.status();
      if (current !== generation.current) return;
      if (status.step_up_valid && status.csrf_token) close(status.csrf_token);
      else setFailed(true);
    } catch {
      if (current === generation.current) setFailed(true);
    } finally {
      // Never keep the secret around after an attempt.
      setPassword('');
      setCode('');
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) close(null); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldCheck className="size-4 text-muted-foreground" aria-hidden="true" />
            {t('restoreFlow.stepUp.title')}
          </DialogTitle>
          <DialogDescription>{t(`restoreFlow.stepUp.why.${purpose}`)}</DialogDescription>
        </DialogHeader>

        {purpose !== 'admin' && (
          <p className="rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">
            {t('restoreFlow.stepUp.audited')}
          </p>
        )}

        {phase === 'loading' && (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner aria-hidden="true" /> {t('restoreFlow.stepUp.checking')}
          </p>
        )}

        {phase === 'load_failed' && (
          <p role="alert" className="text-sm text-destructive">{t('restoreFlow.stepUp.loadFailed')}</p>
        )}

        {phase === 'no_mfa' && (
          <p role="alert" className="text-sm">
            {t('restoreFlow.stepUp.noMfa')}{' '}
            <Link className="underline" to="/me/account" onClick={() => close(null)}>
              {t('restoreFlow.stepUp.goToAccount')}
            </Link>
          </p>
        )}

        {phase === 'valid' && (
          <p role="status" className="text-sm">{t('restoreFlow.stepUp.stillValid')}</p>
        )}

        {phase === 'verify' && (
          <form id={`${passwordId}-form`} className="grid gap-3" onSubmit={(e) => void submit(e)}>
            <div className="grid gap-1.5">
              <label htmlFor={passwordId} className="text-sm font-medium">{t('restoreFlow.stepUp.password')}</label>
              <Input
                id={passwordId}
                type="password"
                autoComplete="current-password"
                maxLength={4096}
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-invalid={failed}
                aria-describedby={failed ? errorId : undefined}
              />
            </div>
            <div className="grid gap-1.5">
              <label htmlFor={codeId} className="text-sm font-medium">{t('restoreFlow.stepUp.code')}</label>
              <Input
                id={codeId}
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={6}
                required
                value={code}
                onChange={(e) => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                aria-invalid={failed}
                aria-describedby={failed ? errorId : undefined}
                className="font-mono tabular-nums"
              />
              <p className="text-xs text-muted-foreground">{t('restoreFlow.stepUp.codeHint')}</p>
            </div>
            {failed && (
              <p id={errorId} role="alert" className="text-sm text-destructive">{t('restoreFlow.stepUp.failed')}</p>
            )}
          </form>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => close(null)}>{t('restoreFlow.common.cancel')}</Button>
          {phase === 'valid' && (
            <Button variant="brand" onClick={() => close(token)}>{t('restoreFlow.stepUp.continue')}</Button>
          )}
          {phase === 'verify' && (
            <Button
              variant="brand"
              type="submit"
              form={`${passwordId}-form`}
              disabled={busy || !password || code.length !== 6}
            >
              {busy ? t('restoreFlow.stepUp.verifying') : t('restoreFlow.stepUp.verify')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
