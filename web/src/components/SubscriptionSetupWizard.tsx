import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, CheckCircle2, ExternalLink, Loader2, RefreshCw, XCircle } from 'lucide-react';
import { api } from '@/lib/api';
import { isImeComposing } from '@/lib/keyboard';
import { QrCode } from '@/components/shared/QrCode';
import { Dialog, DialogContent, DialogHeader, DialogTitle, Button, Input } from '@/components/mds';

interface Props {
  open: boolean;
  onClose: () => void;
  onSuccess?: () => void;
}

type Step = 'starting' | 'awaiting_code' | 'submitting' | 'succeeded' | 'error';

/** The closed set of machine-readable codes `handlers.rs::setup_token_error_frame`
 *  can return — kept in sync manually (small, stable surface; see
 *  `setup_token_wizard.rs::SetupTokenErrorCode`). Any other/unknown code falls
 *  back to the server's own `message` text, then a generic string. */
const KNOWN_ERROR_CODES = [
  'not_installed',
  'no_active_session',
  'expired',
  'invalid_code',
  'timeout',
  'validation_failed',
  'already_submitting',
  'io_error',
] as const;

function errorCodeOf(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'code' in err) {
    const c = (err as { code?: unknown }).code;
    return typeof c === 'string' ? c : undefined;
  }
  return undefined;
}

function messageOf(err: unknown): string | undefined {
  if (err instanceof Error) return err.message;
  if (err && typeof err === 'object' && 'message' in err) {
    const m = (err as { message?: unknown }).message;
    return typeof m === 'string' ? m : undefined;
  }
  return typeof err === 'string' ? err : undefined;
}

/**
 * "連接訂閱帳號" — the WP-D device-code-style setup wizard for headless
 * boxes. A guided, 3-step specialization of {@link CliLoginModal} scoped to
 * Claude subscription (Pro/Max) accounts: this wizard shows a QR code +
 * one-click link for the authorize URL, accepts the pasted-back code, and
 * only reports success once the gateway has verified the resulting
 * credential with a real API call — `CliLoginModal` remains the power-user
 * surface for the other CLIs / advanced accounts.
 */
export function SubscriptionSetupWizard({ open, onClose, onSuccess }: Props) {
  const intl = useIntl();
  const [step, setStep] = useState<Step>('starting');
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [code, setCode] = useState('');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [accountId, setAccountId] = useState<string | null>(null);
  const sidRef = useRef<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const stopPolling = useCallback(() => {
    if (pollRef.current !== null) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  const describeError = useCallback(
    (err: unknown): string => {
      const code = errorCodeOf(err);
      if (code && (KNOWN_ERROR_CODES as readonly string[]).includes(code)) {
        return intl.formatMessage({ id: `subscriptionSetup.error.${code}` });
      }
      return messageOf(err) ?? intl.formatMessage({ id: 'subscriptionSetup.error.unknown' });
    },
    [intl],
  );

  const startFlow = useCallback(() => {
    stopPolling();
    setStep('starting');
    setErrorMsg(null);
    setAuthUrl(null);
    setCode('');
    setAccountId(null);
    sidRef.current = null;

    api.accounts
      .setupTokenStart()
      .then((r) => {
        sidRef.current = r.session_id;
        setAuthUrl(r.auth_url);
        setStep('awaiting_code');
        // The URL is usually ready synchronously; if not, poll status until
        // it appears (or the session ends some other way).
        if (!r.auth_url) {
          pollRef.current = setInterval(() => {
            const sid = sidRef.current;
            if (!sid) return;
            api.accounts
              .setupTokenStatus(sid)
              .then((s) => {
                if (s.auth_url) setAuthUrl(s.auth_url);
                if (s.status !== 'running') stopPolling();
              })
              .catch(() => stopPolling());
          }, 1500);
        }
      })
      .catch((e: unknown) => {
        setStep('error');
        setErrorMsg(describeError(e));
      });
  }, [describeError, stopPolling]);

  useEffect(() => {
    if (!open) return;
    startFlow();
    return () => stopPolling();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- startFlow only on open, not on every identity change
  }, [open]);

  const handleSubmit = useCallback(async () => {
    const sid = sidRef.current;
    if (!sid || !code.trim() || step === 'submitting') return;
    setStep('submitting');
    setErrorMsg(null);
    try {
      const r = await api.accounts.setupTokenSubmit(sid, code.trim());
      setAccountId(r.account_id);
      setStep('succeeded');
      onSuccess?.();
    } catch (e) {
      // Every submit outcome (success or failure) retires the session on the
      // gateway side — a fresh `start` is required, so land on the terminal
      // error step with a "重新開始" action rather than re-showing the code
      // input for a session that no longer exists.
      setStep('error');
      setErrorMsg(describeError(e));
    }
  }, [code, step, describeError, onSuccess]);

  const handleClose = useCallback(async () => {
    stopPolling();
    const sid = sidRef.current;
    if (sid && (step === 'awaiting_code' || step === 'starting')) {
      try {
        await api.accounts.setupTokenCancel(sid);
      } catch {
        /* best-effort */
      }
    }
    onClose();
  }, [onClose, step, stopPolling]);

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) void handleClose(); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'subscriptionSetup.title' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          {step === 'starting' && (
            <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              {intl.formatMessage({ id: 'subscriptionSetup.starting' })}
            </div>
          )}

          {(step === 'awaiting_code' || step === 'submitting') && (
            <>
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'subscriptionSetup.step1.desc' })}
              </p>

              {authUrl ? (
                <div className="flex flex-col items-center gap-3 rounded-lg border border-surface-border bg-muted/20 p-4 sm:flex-row sm:items-start">
                  <QrCode value={authUrl} size={152} />
                  <div className="flex-1 space-y-2">
                    <a
                      href={authUrl}
                      target="_blank"
                      rel="noreferrer"
                      className="inline-flex items-center gap-2 rounded-lg bg-brand px-3 py-2 text-sm font-medium text-brand-foreground transition hover:bg-brand/90"
                    >
                      <ExternalLink className="h-4 w-4" />
                      {intl.formatMessage({ id: 'subscriptionSetup.openLink' })}
                    </a>
                    <p className="text-xs text-muted-foreground">
                      {intl.formatMessage({ id: 'subscriptionSetup.qrHint' })}
                    </p>
                  </div>
                </div>
              ) : (
                <div className="flex items-center gap-2 rounded-lg border border-surface-border bg-muted/20 px-3 py-4 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  {intl.formatMessage({ id: 'subscriptionSetup.waitingForLink' })}
                </div>
              )}

              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'subscriptionSetup.codeLabel' })}
                </label>
                <div className="flex items-center gap-2">
                  <Input
                    value={code}
                    onChange={(e) => setCode(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && !isImeComposing(e)) {
                        e.preventDefault();
                        void handleSubmit();
                      }
                    }}
                    placeholder={intl.formatMessage({ id: 'subscriptionSetup.codePlaceholder' })}
                    disabled={step === 'submitting'}
                    autoComplete="off"
                    spellCheck={false}
                  />
                  <Button
                    variant="brand"
                    onClick={() => void handleSubmit()}
                    disabled={step === 'submitting' || !code.trim()}
                  >
                    {step === 'submitting' ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      intl.formatMessage({ id: 'subscriptionSetup.submit' })
                    )}
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'subscriptionSetup.codeHint' })}
                </p>
              </div>
            </>
          )}

          {step === 'succeeded' && (
            <div className="flex flex-col items-center gap-2 py-6 text-center">
              <CheckCircle2 className="h-8 w-8 text-success" />
              <p className="text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'subscriptionSetup.succeeded' })}
              </p>
              {accountId && <p className="font-mono text-xs text-muted-foreground">{accountId}</p>}
            </div>
          )}

          {step === 'error' && (
            <div className="flex flex-col items-center gap-3 py-6 text-center">
              <XCircle className="h-8 w-8 text-destructive" />
              <p className="text-sm text-destructive">{errorMsg}</p>
              <Button variant="outline" size="sm" onClick={startFlow}>
                <RefreshCw className="h-4 w-4" />
                {intl.formatMessage({ id: 'subscriptionSetup.retry' })}
              </Button>
            </div>
          )}

          {errorMsg && step !== 'error' && (
            <div className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{errorMsg}</span>
            </div>
          )}

          <div className="flex items-center justify-end gap-2 pt-1">
            <Button variant="outline" onClick={() => void handleClose()}>
              {intl.formatMessage({ id: step === 'succeeded' ? 'common.done' : 'common.close' })}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
