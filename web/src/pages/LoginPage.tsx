import { useState, useEffect, type FormEvent } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { Card } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Field, controlClass } from '@/components/ui/Field';
import { DuDu } from '@/components/mascot';

type Mode = 'password' | 'otp';
type OtpStep = 'email' | 'code';

export function LoginPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const login = useAuthStore((s) => s.login);
  const otpRequest = useAuthStore((s) => s.otpRequest);
  const otpVerify = useAuthStore((s) => s.otpVerify);
  const firstRunStatus = useAuthStore((s) => s.firstRunStatus);
  const firstRunClaim = useAuthStore((s) => s.firstRunClaim);
  const loading = useAuthStore((s) => s.loading);

  const [mode, setMode] = useState<Mode>('password');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');

  // First-run onboarding: an unclaimed instance lets the first (localhost)
  // visitor SET the admin password in the browser — no console one-time
  // password to copy. `null` = still checking; `true` = show the claim form.
  const [firstRun, setFirstRun] = useState<boolean | null>(null);
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');

  useEffect(() => {
    let active = true;
    firstRunStatus().then((claimable) => {
      if (active) setFirstRun(claimable);
    });
    return () => {
      active = false;
    };
  }, [firstRunStatus]);

  const handleClaim = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    if (newPassword.length < 8) {
      setError(intl.formatMessage({ id: 'login.claim.tooShort' }));
      return;
    }
    if (newPassword !== confirmPassword) {
      setError(intl.formatMessage({ id: 'login.claim.mismatch' }));
      return;
    }
    try {
      await firstRunClaim(newPassword);
      navigate('/', { replace: true });
    } catch {
      setError(intl.formatMessage({ id: 'login.claim.failed' }));
    }
  };

  // OTP sub-flow state.
  const [otpStep, setOtpStep] = useState<OtpStep>('email');
  const [challengeId, setChallengeId] = useState('');
  const [hint, setHint] = useState<string | undefined>(undefined);
  const [code, setCode] = useState('');
  const [otpBusy, setOtpBusy] = useState(false);

  // L35: map raw (English) server errors to localized messages.
  const localizeLoginError = (raw: string): string => {
    const msg = raw.toLowerCase();
    let id = 'login.error.generic';
    if (msg.includes('invalid email or password')) id = 'login.error.invalidCredentials';
    else if (msg.includes('invalid or expired code')) id = 'login.error.otpInvalid';
    else if (msg.includes('too many')) id = 'login.error.tooManyAttempts';
    else if (msg.includes('token generation failed') || msg.includes('http 5')) id = 'login.error.serverError';
    else if (msg.includes('failed to fetch') || msg.includes('networkerror')) id = 'login.error.network';
    return intl.formatMessage({ id });
  };

  const handlePasswordSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    try {
      await login(email, password);
      navigate('/', { replace: true });
    } catch (err) {
      setError(localizeLoginError(err instanceof Error ? err.message : String(err)));
    }
  };

  const handleOtpRequest = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    setOtpBusy(true);
    try {
      const res = await otpRequest(email);
      setChallengeId(res.challenge_id);
      setHint(res.hint);
      setOtpStep('code');
    } catch (err) {
      setError(localizeLoginError(err instanceof Error ? err.message : String(err)));
    } finally {
      setOtpBusy(false);
    }
  };

  const handleOtpVerify = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    try {
      await otpVerify(challengeId, code);
      navigate('/', { replace: true });
    } catch (err) {
      setError(localizeLoginError(err instanceof Error ? err.message : String(err)));
    }
  };

  const switchMode = (next: Mode) => {
    setMode(next);
    setError('');
    setOtpStep('email');
    setCode('');
  };

  return (
    <div className="relative flex min-h-screen items-center justify-center p-4">
      <div className="app-ambient" aria-hidden="true" />

      <div className="page-enter w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center text-center">
          {/* DuDu greets the returning operator with a wave (§7.3 接待員). */}
          <DuDu face="waving" size={96} label="DuDu" />
          <h1 className="mt-3 text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'login.subtitle' })}
          </p>
        </div>

        <Card>
          {error && (
            <div className="mb-4 rounded-lg border border-rose-400/30 bg-rose-50/80 px-4 py-3 text-sm text-rose-700 dark:bg-rose-950/50 dark:text-rose-300">
              {error}
            </div>
          )}

          {firstRun ? (
            <form onSubmit={handleClaim}>
              <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'login.claim.intro' })}
              </p>
              <div className="space-y-4">
                <Field label={intl.formatMessage({ id: 'login.claim.newPassword' })} htmlFor="claim-pw">
                  <input id="claim-pw" type="password" autoComplete="new-password" required minLength={8}
                    value={newPassword} onChange={(e) => setNewPassword(e.target.value)}
                    className={controlClass} placeholder="••••••••" />
                </Field>
                <Field label={intl.formatMessage({ id: 'login.claim.confirm' })} htmlFor="claim-confirm">
                  <input id="claim-confirm" type="password" autoComplete="new-password" required minLength={8}
                    value={confirmPassword} onChange={(e) => setConfirmPassword(e.target.value)}
                    className={controlClass} placeholder="••••••••" />
                </Field>
              </div>
              <Button type="submit" variant="primary" disabled={loading || newPassword.length < 8}
                className="mt-6 h-10 w-full disabled:cursor-not-allowed">
                {loading ? intl.formatMessage({ id: 'login.loading' }) : intl.formatMessage({ id: 'login.claim.submit' })}
              </Button>
            </form>
          ) : mode === 'password' ? (
            <form onSubmit={handlePasswordSubmit}>
              <div className="space-y-4">
                <Field label={intl.formatMessage({ id: 'login.email' })} htmlFor="email">
                  <input id="email" type="email" autoComplete="email" required value={email}
                    onChange={(e) => setEmail(e.target.value)} className={controlClass} placeholder="admin@local" />
                </Field>
                <Field label={intl.formatMessage({ id: 'login.password' })} htmlFor="password">
                  <input id="password" type="password" autoComplete="current-password" required value={password}
                    onChange={(e) => setPassword(e.target.value)} className={controlClass} />
                </Field>
              </div>
              <Button type="submit" variant="primary" disabled={loading} className="mt-6 h-10 w-full disabled:cursor-not-allowed">
                {loading ? intl.formatMessage({ id: 'login.loading' }) : intl.formatMessage({ id: 'login.submit' })}
              </Button>
              <button type="button" onClick={() => switchMode('otp')}
                className="mt-4 w-full text-center text-sm text-amber-600 transition-colors hover:text-amber-700 dark:text-amber-400">
                {intl.formatMessage({ id: 'login.useChannelCode' })}
              </button>
            </form>
          ) : otpStep === 'email' ? (
            <form onSubmit={handleOtpRequest}>
              <div className="space-y-4">
                <Field label={intl.formatMessage({ id: 'login.email' })} htmlFor="otp-email">
                  <input id="otp-email" type="email" autoComplete="email" required value={email}
                    onChange={(e) => setEmail(e.target.value)} className={controlClass} placeholder="you@company.com" />
                </Field>
              </div>
              <Button type="submit" variant="primary" disabled={otpBusy} className="mt-6 h-10 w-full disabled:cursor-not-allowed">
                {otpBusy ? intl.formatMessage({ id: 'login.loading' }) : intl.formatMessage({ id: 'login.otp.sendCode' })}
              </Button>
              <button type="button" onClick={() => switchMode('password')}
                className="mt-4 w-full text-center text-sm text-stone-500 transition-colors hover:text-stone-700 dark:text-stone-400">
                {intl.formatMessage({ id: 'login.usePassword' })}
              </button>
            </form>
          ) : (
            <form onSubmit={handleOtpVerify}>
              <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
                {hint
                  ? intl.formatMessage({ id: 'login.otp.sentTo' }, { target: hint })
                  : intl.formatMessage({ id: 'login.otp.sentGeneric' })}
              </p>
              <Field label={intl.formatMessage({ id: 'login.otp.codeLabel' })} htmlFor="otp-code">
                <input id="otp-code" inputMode="numeric" autoComplete="one-time-code" required value={code}
                  onChange={(e) => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                  className={controlClass} placeholder="000000" maxLength={6} />
              </Field>
              <Button type="submit" variant="primary" disabled={loading || code.length < 6}
                className="mt-6 h-10 w-full disabled:cursor-not-allowed">
                {loading ? intl.formatMessage({ id: 'login.loading' }) : intl.formatMessage({ id: 'login.otp.verify' })}
              </Button>
              <div className="mt-4 flex items-center justify-between text-sm">
                <button type="button" onClick={() => setOtpStep('email')}
                  className="text-stone-500 transition-colors hover:text-stone-700 dark:text-stone-400">
                  {intl.formatMessage({ id: 'login.otp.back' })}
                </button>
                <button type="button" onClick={() => switchMode('password')}
                  className="text-stone-500 transition-colors hover:text-stone-700 dark:text-stone-400">
                  {intl.formatMessage({ id: 'login.usePassword' })}
                </button>
              </div>
            </form>
          )}
        </Card>

        {!firstRun && (
          <p className="mt-6 text-center text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'login.footer' })}
          </p>
        )}
      </div>
    </div>
  );
}
