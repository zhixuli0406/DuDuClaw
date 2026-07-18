import { useState, useEffect, type FormEvent } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { Card, Button, Input, Spinner } from '@/components/mds';
import { Field } from '@/components/onboarding';
import { DuDu } from '@/components/mascot';
import { useEffectiveName, useEffectiveLogo } from '@/lib/branding';

type Mode = 'password' | 'otp';
type OtpStep = 'email' | 'code';

export function LoginPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  // Pre-auth branding comes from the localStorage cache the store hydrates at
  // module load; a white-label distributor sees their brand on the login page.
  const brandName = useEffectiveName();
  const brandLogo = useEffectiveLogo();
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

  // A brand button that renders a static-frame spinner while busy (login's
  // three states: idle / submitting / disabled).
  const submitLabel = (busy: boolean, busyId: string, id: string) =>
    busy ? (
      <>
        <Spinner label={intl.formatMessage({ id: 'login.loading' })} />
        {intl.formatMessage({ id: busyId })}
      </>
    ) : (
      intl.formatMessage({ id })
    );

  return (
    <div className="flex min-h-screen items-center justify-center bg-app-shell p-4">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex flex-col items-center text-center">
          {/* A white-label distributor with a custom logo shows it here;
              otherwise DuDu greets the returning operator (§7.3 接待員). */}
          {brandLogo.isImage ? (
            <img
              src={brandLogo.value}
              alt={brandName}
              className="h-16 w-16 rounded-xl object-cover ring-1 ring-surface-border"
            />
          ) : (
            <DuDu face="waving" size={72} label="DuDu" />
          )}
          <h1 className="mt-3 text-base font-medium text-foreground">{brandName}</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'login.subtitle' })}
          </p>
        </div>

        <Card className="gap-5 p-6">
          {error && (
            <div
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {error}
            </div>
          )}

          {firstRun ? (
            <form onSubmit={handleClaim} className="space-y-4">
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'login.claim.intro' })}
              </p>
              <Field label={intl.formatMessage({ id: 'login.claim.newPassword' })} htmlFor="claim-pw">
                <Input
                  id="claim-pw"
                  type="password"
                  autoComplete="new-password"
                  required
                  minLength={8}
                  value={newPassword}
                  onChange={(e) => setNewPassword(e.target.value)}
                  placeholder="••••••••"
                />
              </Field>
              <Field label={intl.formatMessage({ id: 'login.claim.confirm' })} htmlFor="claim-confirm">
                <Input
                  id="claim-confirm"
                  type="password"
                  autoComplete="new-password"
                  required
                  minLength={8}
                  value={confirmPassword}
                  onChange={(e) => setConfirmPassword(e.target.value)}
                  placeholder="••••••••"
                />
              </Field>
              <Button
                type="submit"
                variant="brand"
                size="lg"
                disabled={loading || newPassword.length < 8}
                className="w-full"
              >
                {submitLabel(loading, 'login.loading', 'login.claim.submit')}
              </Button>
            </form>
          ) : mode === 'password' ? (
            <form onSubmit={handlePasswordSubmit} className="space-y-4">
              <Field label={intl.formatMessage({ id: 'login.email' })} htmlFor="email">
                <Input
                  id="email"
                  type="email"
                  autoComplete="email"
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="admin@local"
                />
              </Field>
              <Field label={intl.formatMessage({ id: 'login.password' })} htmlFor="password">
                <Input
                  id="password"
                  type="password"
                  autoComplete="current-password"
                  required
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </Field>
              <Button type="submit" variant="brand" size="lg" disabled={loading} className="w-full">
                {submitLabel(loading, 'login.loading', 'login.submit')}
              </Button>
              <Button
                type="button"
                variant="link"
                onClick={() => switchMode('otp')}
                className="w-full text-brand"
              >
                {intl.formatMessage({ id: 'login.useChannelCode' })}
              </Button>
            </form>
          ) : otpStep === 'email' ? (
            <form onSubmit={handleOtpRequest} className="space-y-4">
              <Field label={intl.formatMessage({ id: 'login.email' })} htmlFor="otp-email">
                <Input
                  id="otp-email"
                  type="email"
                  autoComplete="email"
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="you@company.com"
                />
              </Field>
              <Button type="submit" variant="brand" size="lg" disabled={otpBusy} className="w-full">
                {submitLabel(otpBusy, 'login.loading', 'login.otp.sendCode')}
              </Button>
              <Button
                type="button"
                variant="link"
                onClick={() => switchMode('password')}
                className="w-full text-muted-foreground"
              >
                {intl.formatMessage({ id: 'login.usePassword' })}
              </Button>
            </form>
          ) : (
            <form onSubmit={handleOtpVerify} className="space-y-4">
              <p className="text-sm text-muted-foreground">
                {hint
                  ? intl.formatMessage({ id: 'login.otp.sentTo' }, { target: hint })
                  : intl.formatMessage({ id: 'login.otp.sentGeneric' })}
              </p>
              <Field label={intl.formatMessage({ id: 'login.otp.codeLabel' })} htmlFor="otp-code">
                <Input
                  id="otp-code"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  required
                  value={code}
                  onChange={(e) => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                  placeholder="000000"
                  maxLength={6}
                />
              </Field>
              <Button
                type="submit"
                variant="brand"
                size="lg"
                disabled={loading || code.length < 6}
                className="w-full"
              >
                {submitLabel(loading, 'login.loading', 'login.otp.verify')}
              </Button>
              <div className="flex items-center justify-between">
                <Button
                  type="button"
                  variant="link"
                  onClick={() => setOtpStep('email')}
                  className="text-muted-foreground"
                >
                  {intl.formatMessage({ id: 'login.otp.back' })}
                </Button>
                <Button
                  type="button"
                  variant="link"
                  onClick={() => switchMode('password')}
                  className="text-muted-foreground"
                >
                  {intl.formatMessage({ id: 'login.usePassword' })}
                </Button>
              </div>
            </form>
          )}
        </Card>

        {!firstRun && (
          <p className="mt-6 text-center text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'login.footer' })}
          </p>
        )}
      </div>
    </div>
  );
}
