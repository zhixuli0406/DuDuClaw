import { useState, type FormEvent } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { inputClass, buttonPrimary } from '@/components/shared/Dialog';

export function LoginPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const login = useAuthStore((s) => s.login);
  const loading = useAuthStore((s) => s.loading);

  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');

  // L35 fix: map raw (English) server errors to localized, user-facing
  // messages. Server strings come from the gateway /api/login handler; any
  // unmatched error falls back to a generic localized message so we never
  // surface raw English / HTTP-status text to the user.
  const localizeLoginError = (raw: string): string => {
    const msg = raw.toLowerCase();
    let id = 'login.error.generic';
    if (msg.includes('invalid email or password')) {
      id = 'login.error.invalidCredentials';
    } else if (msg.includes('too many login attempts')) {
      id = 'login.error.tooManyAttempts';
    } else if (msg.includes('token generation failed') || msg.includes('http 5')) {
      id = 'login.error.serverError';
    } else if (msg.includes('failed to fetch') || msg.includes('networkerror')) {
      id = 'login.error.network';
    }
    return intl.formatMessage({ id });
  };

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    try {
      await login(email, password);
      navigate('/', { replace: true });
    } catch (err) {
      const raw = err instanceof Error ? err.message : String(err);
      setError(localizeLoginError(raw));
    }
  };

  return (
    <div className="relative flex min-h-screen items-center justify-center p-4">
      <div className="app-ambient" aria-hidden="true" />

      <div className="page-enter w-full max-w-sm">
        {/* Logo */}
        <div className="mb-8 text-center">
          <span
            className="inline-grid h-16 w-16 place-items-center rounded-2xl bg-gradient-to-b from-amber-400 to-amber-500 text-3xl shadow-[0_8px_32px_-8px_rgba(245,158,11,0.7),inset_0_1px_0_0_rgba(255,255,255,0.4)]"
            role="img"
            aria-label="paw"
          >
            🐾
          </span>
          <h1 className="mt-4 text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'login.subtitle' })}
          </p>
        </div>

        {/* Login Form */}
        <form onSubmit={handleSubmit} className="glass-overlay rounded-2xl p-6">
          {error && (
            <div className="mb-4 rounded-lg border border-rose-400/30 bg-rose-50/80 px-4 py-3 text-sm text-rose-700 dark:bg-rose-950/50 dark:text-rose-300">
              {error}
            </div>
          )}

          <div className="space-y-4">
            <div>
              <label
                htmlFor="email"
                className="block text-sm font-medium text-stone-700 dark:text-stone-300"
              >
                {intl.formatMessage({ id: 'login.email' })}
              </label>
              <input
                id="email"
                type="email"
                autoComplete="email"
                required
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                className={`mt-1 ${inputClass}`}
                placeholder="admin@local"
              />
            </div>

            <div>
              <label
                htmlFor="password"
                className="block text-sm font-medium text-stone-700 dark:text-stone-300"
              >
                {intl.formatMessage({ id: 'login.password' })}
              </label>
              <input
                id="password"
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className={`mt-1 ${inputClass}`}
              />
            </div>
          </div>

          <button
            type="submit"
            disabled={loading}
            className={`mt-6 w-full ${buttonPrimary} py-2.5 disabled:cursor-not-allowed`}
          >
            {loading
              ? intl.formatMessage({ id: 'login.loading' })
              : intl.formatMessage({ id: 'login.submit' })}
          </button>
        </form>

        <p className="mt-6 text-center text-xs text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'login.footer' })}
        </p>
      </div>
    </div>
  );
}
