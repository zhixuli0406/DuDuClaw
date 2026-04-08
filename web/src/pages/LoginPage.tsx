import { useState, type FormEvent } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';

export function LoginPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const login = useAuthStore((s) => s.login);
  const loading = useAuthStore((s) => s.loading);

  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    try {
      await login(email, password);
      navigate('/', { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div className="flex min-h-screen items-center justify-center bg-stone-50 dark:bg-stone-950">
      <div className="w-full max-w-sm">
        {/* Logo */}
        <div className="mb-8 text-center">
          <span className="text-5xl" role="img" aria-label="paw">
            🐾
          </span>
          <h1 className="mt-4 text-2xl font-semibold text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'login.subtitle' })}
          </p>
        </div>

        {/* Login Form */}
        <form
          onSubmit={handleSubmit}
          className="rounded-xl border border-stone-200 bg-white p-6 shadow-sm dark:border-stone-800 dark:bg-stone-900"
        >
          {error && (
            <div className="mb-4 rounded-lg bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:bg-rose-900/20 dark:text-rose-400">
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
                className="mt-1 block w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm shadow-sm placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
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
                className="mt-1 block w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm shadow-sm placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
              />
            </div>
          </div>

          <button
            type="submit"
            disabled={loading}
            className="mt-6 w-full rounded-lg bg-amber-500 px-4 py-2.5 text-sm font-medium text-white shadow-sm transition-colors hover:bg-amber-600 focus:outline-none focus:ring-2 focus:ring-amber-500 focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 dark:focus:ring-offset-stone-900"
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
