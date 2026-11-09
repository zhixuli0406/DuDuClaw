import { createContext, useCallback, useContext, useMemo, useState, type FormEvent, type ReactNode } from 'react';
import { Link } from 'react-router';
import { api, type ProtectedStepUpState } from '@/lib/api';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from '@/components/mds';
import { Refusal, refusalMessage, useT } from './shared';

/**
 * Step-up (re-verify identity) for the protected-mode page.
 *
 * The proof is the existing five-minute dashboard step-up
 * (`isolation.connector_step_up`, same as `migration.confirmations.step_up`):
 * password + authenticator code, bound to this session. The list RPCs report
 * whether it is still valid and hand back its CSRF token; every mutation
 * sends that token and the server re-checks all of it. Nothing here decides
 * authority — a stale view only means the server refuses and we show why.
 */
export interface StepUpStatus {
  valid: boolean;
  csrf: number[] | null;
  mfaEnrolled: boolean;
}

interface StepUpController {
  status: StepUpStatus;
  /** Record the step-up fields of a list response. */
  record: (state: ProtectedStepUpState) => void;
  /** Re-read the proof state from the server. */
  refresh: () => Promise<StepUpStatus>;
  /** Verify password + code, then return the fresh CSRF token. */
  verify: (password: string, code: string) => Promise<number[]>;
}

const INITIAL: StepUpStatus = { valid: false, csrf: null, mfaEnrolled: true };

const StepUpContext = createContext<StepUpController | null>(null);

function fromState(state: ProtectedStepUpState): StepUpStatus {
  const csrf = Array.isArray(state.csrf_token) && state.csrf_token.length === 32 ? state.csrf_token : null;
  return { valid: state.step_up_valid === true && csrf !== null, csrf, mfaEnrolled: state.mfa_enrolled !== false };
}

export class StepUpUnavailable extends Error {}

export function StepUpProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<StepUpStatus>(INITIAL);
  const record = useCallback((state: ProtectedStepUpState) => setStatus(fromState(state)), []);
  const refresh = useCallback(async () => {
    const next = fromState(await api.protectedMode.employeeList());
    setStatus(next);
    return next;
  }, []);
  const verify = useCallback(async (password: string, code: string) => {
    await api.protectedMode.stepUp(password, code);
    const next = await refresh();
    if (!next.valid || !next.csrf) throw new StepUpUnavailable();
    return next.csrf;
  }, [refresh]);
  const value = useMemo(() => ({ status, record, refresh, verify }), [status, record, refresh, verify]);
  return <StepUpContext.Provider value={value}>{children}</StepUpContext.Provider>;
}

export function useStepUp(): StepUpController {
  const controller = useContext(StepUpContext);
  if (!controller) throw new Error('useStepUp outside StepUpProvider');
  return controller;
}

/** Runs one state-changing action behind the step-up. */
export function useStepUpAction() {
  const t = useT();
  const controller = useStepUp();
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const needsVerify = !controller.status.valid;
  const mfaMissing = !controller.status.mfaEnrolled;
  const canSubmit = !busy && !mfaMissing && (!needsVerify || (password.length > 0 && /^[0-9]{6}$/.test(code)));

  const run = useCallback(async (action: (csrf: number[]) => Promise<void>): Promise<boolean> => {
    if (busy || mfaMissing) return false;
    setBusy(true);
    setError(null);
    try {
      let csrf = controller.status.valid ? controller.status.csrf : null;
      if (!csrf) {
        try {
          csrf = await controller.verify(password, code);
        } catch (e) {
          setError(e instanceof StepUpUnavailable ? t('stepUp.notVerified') : refusalMessage(e));
          return false;
        }
      }
      await action(csrf);
      return true;
    } catch (e) {
      setError(refusalMessage(e));
      // The refusal may be an expired proof; re-read so the fields come back.
      void controller.refresh().catch(() => {});
      return false;
    } finally {
      // The password and code are never kept after an attempt.
      setPassword('');
      setCode('');
      setBusy(false);
    }
  }, [busy, mfaMissing, controller, password, code, t]);

  const reset = useCallback(() => {
    setPassword('');
    setCode('');
    setError(null);
  }, []);

  return { needsVerify, mfaMissing, password, setPassword, code, setCode, busy, error, canSubmit, run, reset };
}

type StepUpAction = ReturnType<typeof useStepUpAction>;

/** Password + code fields, shown only while the proof is not valid. */
export function StepUpFields({ action, idPrefix }: { action: StepUpAction; idPrefix: string }) {
  const t = useT();
  if (action.mfaMissing) {
    return (
      <p role="alert" className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-sm">
        {t('stepUp.mfaMissing')}{' '}
        <Link to="/me/account" className="underline">{t('stepUp.mfaLink')}</Link>
      </p>
    );
  }
  if (!action.needsVerify) {
    return <p className="text-sm text-muted-foreground">{t('stepUp.stillValid')}</p>;
  }
  return (
    <fieldset className="space-y-3 rounded-lg border border-surface-border p-3">
      <legend className="px-1 text-sm font-medium">{t('stepUp.legend')}</legend>
      <p className="text-sm text-muted-foreground">{t('stepUp.explain')}</p>
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1">
          <label htmlFor={`${idPrefix}-password`} className="block text-sm">{t('stepUp.password')}</label>
          <Input
            id={`${idPrefix}-password`}
            type="password"
            autoComplete="current-password"
            maxLength={4096}
            required
            value={action.password}
            onChange={(e) => action.setPassword(e.target.value)}
          />
        </div>
        <div className="space-y-1">
          <label htmlFor={`${idPrefix}-code`} className="block text-sm">{t('stepUp.code')}</label>
          <Input
            id={`${idPrefix}-code`}
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]{6}"
            maxLength={6}
            required
            value={action.code}
            onChange={(e) => action.setCode(e.target.value.replace(/\D/g, ''))}
          />
        </div>
      </div>
    </fieldset>
  );
}

/**
 * A confirmation dialog whose confirm button re-verifies identity first when
 * the proof has lapsed ("再驗證身分並…"), then runs `onConfirm`.
 */
export function StepUpDialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  confirmLabel,
  verifyConfirmLabel,
  destructive = false,
  disabled = false,
  onConfirm,
  idPrefix,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children?: ReactNode;
  confirmLabel: string;
  verifyConfirmLabel: string;
  destructive?: boolean;
  /** Extra precondition from the caller's own fields. */
  disabled?: boolean;
  onConfirm: (csrf: number[]) => Promise<void>;
  idPrefix: string;
}) {
  const t = useT();
  const action = useStepUpAction();
  const close = (next: boolean) => {
    if (!next) action.reset();
    onOpenChange(next);
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (disabled || !action.canSubmit) return;
    if (await action.run(onConfirm)) close(false);
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <form onSubmit={(e) => void submit(e)} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          {children}
          <StepUpFields action={action} idPrefix={idPrefix} />
          <Refusal message={action.error} />
          <DialogFooter>
            <Button variant="outline" onClick={() => close(false)} disabled={action.busy}>
              {t('cancel')}
            </Button>
            <Button
              type="submit"
              variant={destructive ? 'destructive' : 'brand'}
              className={destructive ? 'bg-destructive text-white hover:bg-destructive/90' : undefined}
              disabled={disabled || !action.canSubmit}
            >
              {action.needsVerify ? verifyConfirmLabel : confirmLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
