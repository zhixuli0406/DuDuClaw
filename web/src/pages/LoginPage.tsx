import { dashboardEndpoint } from '@/lib/dashboard-base';
import { LogIn } from 'lucide-react';
import { approvalConfirmReturn } from '@/lib/approval-step-up';
import { googleAuthorizationReturn } from '@/lib/google-authorization';
import { useState, useEffect, useRef, type FormEvent } from 'react';
import { useLocation, useNavigate } from 'react-router';
import { browserWatchReturn } from '@/lib/browser-watch-return';
import { useIntl } from 'react-intl';
import { LoginMfa } from '@/components/mfa/LoginMfa';
import { apiPost, useAuthStore, type LoginChallenge } from '@/stores/auth-store';
import { Card, Button, Input, Spinner } from '@/components/mds';
import { Field } from '@/components/onboarding';
import { ProductMascot as DuDu } from '@/components/brand/ProductLogo';
import { CompanyLogo, useCompanyName } from '@/components/brand/CompanyLogo';
import { useCompanyStore } from '@/stores/company-store';
import { useEffectiveName } from '@/lib/product-identity';

type Mode = 'password' | 'otp';
type OtpStep = 'email' | 'code';

const googleResultKeys = new Map([
  ['domain_not_allowed', 'domainNotAllowed'], ['personal_account', 'personalAccount'],
  ['existing_account', 'existingAccount'], ['pending_approval', 'pendingApproval'],
  ['account_disabled', 'cannotSignIn'], ['rejected', 'cannotSignIn'],
  ['daily_limit', 'dailyLimit'],
]);

export function LoginPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const location = useLocation();
  const from = (location.state as { from?: unknown } | null)?.from;
  const googleReturn = useRef(/^#gsi=[A-Za-z0-9_-]+$/.test(window.location.hash)
    || window.location.hash === '#gsi_error=expired');
  const returnTo = googleReturn.current ? '/' : googleAuthorizationReturn(from) ?? approvalConfirmReturn(from) ?? browserWatchReturn(from) ?? '/';
  const brandName = useEffectiveName();
  const companyName = useCompanyName();
  const company = useCompanyStore((state) => state.company);
  const hasCompany = Boolean(company.name || company.logo_url);
  const googleComplete = useAuthStore((s) => s.googleComplete);
  const login = useAuthStore((s) => s.login);
  const otpRequest = useAuthStore((s) => s.otpRequest);
  const otpVerify = useAuthStore((s) => s.otpVerify);
  const firstRunStatus = useAuthStore((s) => s.firstRunStatus);
  const firstRunClaim = useAuthStore((s) => s.firstRunClaim);
  const tryLocalSession = useAuthStore((s) => s.tryLocalSession);
  const loading = useAuthStore((s) => s.loading);

  const [mfa, setMfa] = useState<LoginChallenge | null>(null);
  const [mode, setMode] = useState<Mode>('password');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [remember, setRemember] = useState(false);
  const [error, setError] = useState('');
  const [googleAvailable, setGoogleAvailable] = useState(false);
  const [googleMessage, setGoogleMessage] = useState('');
  const [googleCompleting, setGoogleCompleting] = useState(googleReturn.current
    && window.location.hash !== '#gsi_error=expired');
  const [googleStarting, setGoogleStarting] = useState(false);
  const googleHandled = useRef(false);
  const googleStartPending = useRef(false);

  useEffect(() => {
    let active = true;
    void fetch(dashboardEndpoint('/api/login/methods'), { credentials: 'same-origin', cache: 'no-store' })
      .then(async (response) => {
        if (!response.ok) return;
        const data = await response.json();
        if (active) setGoogleAvailable(data?.google === 'available');
      }).catch(() => { /* Unavailable methods stay hidden. */ });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (!googleReturn.current || googleHandled.current) return;
    googleHandled.current = true;
    const fragment = window.location.hash;
    // Preserve React Router's history state; the ticket only enters the POST body.
    window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search);
    if (fragment === '#gsi_error=expired') {
      setGoogleMessage('incomplete');
      return;
    }
    void googleComplete(fragment.slice('#gsi='.length)).then((result) => {
      if (result && 'result' in result) setGoogleMessage(googleResultKeys.get(result.result) ?? 'incomplete');
      else if (result) setMfa(result);
      else navigate('/', { replace: true });
    }).catch(() => setGoogleMessage('incomplete'))
      .finally(() => setGoogleCompleting(false));
  }, [googleComplete, navigate]);

  const handleGoogleStart = async () => {
    if (googleStartPending.current) return;
    googleStartPending.current = true;
    setGoogleStarting(true);
    setGoogleMessage('');
    try {
      const data = await apiPost<{ authorization_url: string }>('/api/login/google/start', {});
      const url = new URL(data.authorization_url);
      if (url.origin !== 'https://accounts.google.com' || url.username || url.password) throw new Error('Invalid authorization origin');
      window.location.assign(url.href);
    } catch {
      setGoogleMessage('incomplete');
      googleStartPending.current = false;
      setGoogleStarting(false);
    }
  };

  // First-run onboarding: an unclaimed instance lets the first (localhost)
  // visitor SET the admin password in the browser — no console one-time
  // password to copy. `null` = still checking; `true` = show the claim form.
  const [firstRun, setFirstRun] = useState<boolean | null>(null);
  const [ownerEmail, setOwnerEmail] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');

  useEffect(() => {
    if (googleReturn.current) { setFirstRun(false); return; }
    let active = true;
    firstRunStatus().then((claimable) => {
      if (active) setFirstRun(claimable);
    });
    return () => {
      active = false;
    };
  }, [firstRunStatus]);

  // WP-F1 (design §2, D3-A). Landing here directly — a bookmarked /login, or a
  // session that expired — must not dead-end a passwordless Personal install
  // whose owner never set a password. AuthGuard already probes once per page
  // load; the store's one-shot flag makes this a no-op when it did, so a
  // refused instance shows the form immediately with no extra request.
  useEffect(() => {
    if (googleReturn.current) return;
    let active = true;
    void tryLocalSession().then((ok) => {
      if (ok && active) navigate(returnTo, { replace: true });
    });
    return () => {
      active = false;
    };
  }, [tryLocalSession, navigate, returnTo]);

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
      const result = await firstRunClaim(ownerEmail, newPassword);
      setFirstRun(false);
      if (result) setMfa(result);
      else navigate('/', { replace: true });
    } catch (err) {
      const msg = err instanceof Error ? err.message.toLowerCase() : '';
      let id = 'login.claim.failed';
      if (msg.includes('already in use')) id = 'login.claim.emailTaken';
      else if (msg.includes('email')) id = 'login.claim.invalidEmail';
      setError(intl.formatMessage({ id }));
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
      const result = await login(email, password, remember);
      setPassword('');
      if (result) setMfa(result);
      else navigate(returnTo, { replace: true });
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
      const result = await otpVerify(challengeId, code, remember);
      setCode(''); setChallengeId('');
      if (result) setMfa(result);
      else navigate(returnTo, { replace: true });
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
          {hasCompany ? <CompanyLogo size={72} /> : <DuDu face="waving" size={72} label="DuDu" />}
          <h1 className="mt-3 text-base font-medium text-foreground">{companyName}</h1>
          <p className="mt-1 text-sm text-muted-foreground" data-testid="login-subtitle">
            {intl.formatMessage({ id: 'login.subtitle' })}
          </p>
        </div>

        <Card className="gap-5 p-6">
          {approvalConfirmReturn(returnTo) && <p role="note" className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'approvalConfirm.loginNote' })}</p>}
          {error && (
            <div
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              {error}
            </div>
          )}

          {!googleCompleting && !mfa && !firstRun && <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={remember} onChange={e => setRemember(e.target.checked)} />
            {intl.formatMessage({ id: 'login.remember' })}
          </label>}
          {googleCompleting ? (
            <div role="status" className="space-y-2 py-8 text-center">
              <p>{intl.formatMessage({ id: 'login.google.completing' })}</p>
              <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'login.google.wait' })}</p>
            </div>
          ) : mfa ? (
            <LoginMfa challenge={mfa} onComplete={() => navigate(returnTo, { replace: true })}
              onCancel={() => { setMfa(null); setFirstRun(false); setOtpStep('email'); setError(''); }} />
          ) : firstRun ? (
            <form onSubmit={handleClaim} className="space-y-4">
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'login.claim.intro' })}
              </p>
              <Field label={intl.formatMessage({ id: 'login.claim.email' })} htmlFor="claim-email">
                <Input
                  id="claim-email"
                  type="email"
                  autoComplete="email"
                  required
                  value={ownerEmail}
                  onChange={(e) => setOwnerEmail(e.target.value)}
                  placeholder="you@company.com"
                />
              </Field>
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
                disabled={loading || ownerEmail.trim() === '' || newPassword.length < 8}
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
                  placeholder="you@company.com"
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
          {!googleCompleting && !mfa && !firstRun && (googleAvailable || googleMessage) && (
            <div className="space-y-4">
              {googleAvailable && <div className="flex items-center gap-3 text-sm text-muted-foreground">
                <span className="h-px flex-1 bg-border" />
                {intl.formatMessage({ id: 'login.google.or' })}
                <span className="h-px flex-1 bg-border" />
              </div>}
              {googleMessage && <p role="alert" className="rounded-lg border border-border bg-muted px-3 py-2 text-sm">
                {intl.formatMessage({ id: `login.google.result.${googleMessage}` })}
              </p>}
              {googleAvailable && <Button type="button" variant="outline" size="lg" className="w-full"
                disabled={googleStarting || loading || otpBusy} onClick={handleGoogleStart}>
                <LogIn size={16} aria-hidden="true" />
                {intl.formatMessage({ id: 'login.google.button' })}
              </Button>}
            </div>
          )}
        </Card>

        {hasCompany && <p className="mt-6 text-center text-xs text-muted-foreground">{brandName}</p>}
        {!firstRun && (
          <p className="mt-6 text-center text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'login.footer' })}
          </p>
        )}
      </div>
    </div>
  );
}
