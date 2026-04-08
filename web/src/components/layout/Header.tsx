import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useUpdateStore } from '@/stores/update-store';
import { useAuthStore } from '@/stores/auth-store';
import { Sun, Moon, Monitor, RefreshCw, ArrowUpCircle, X } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useState, useCallback } from 'react';

type Theme = 'light' | 'dark' | 'system';

export function Header() {
  const intl = useIntl();
  const navigate = useNavigate();
  const connectionState = useConnectionStore((s) => s.state);
  const connectionError = useConnectionStore((s) => s.error);
  const connectWithAuth = useConnectionStore((s) => s.connectWithAuth);
  const reconnect = useCallback(
    () => connectWithAuth(() => useAuthStore.getState().jwt ?? undefined),
    [connectWithAuth]
  );
  const updateNotification = useUpdateStore((s) => s.notification);
  const updateDismissed = useUpdateStore((s) => s.dismissed);
  const dismissUpdate = useUpdateStore((s) => s.dismiss);
  const checkUpdate = useUpdateStore((s) => s.checkNow);
  const [theme, setTheme] = useState<Theme>('system');

  // Check for updates on mount (in case gateway already has cached info)
  useEffect(() => {
    if (connectionState === 'authenticated') {
      checkUpdate();
    }
  }, [connectionState, checkUpdate]);

  useEffect(() => {
    const stored = localStorage.getItem('duduclaw-theme') as Theme | null;
    if (stored) setTheme(stored);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    const applyTheme = () => {
      if (theme === 'system') {
        root.classList.toggle(
          'dark',
          window.matchMedia('(prefers-color-scheme: dark)').matches
        );
        localStorage.removeItem('duduclaw-theme');
      } else {
        root.classList.toggle('dark', theme === 'dark');
        localStorage.setItem('duduclaw-theme', theme);
      }
    };
    applyTheme();

    // Listen for OS theme changes when in system mode (FE-M5)
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const listener = () => { if (theme === 'system') applyTheme(); };
    mq.addEventListener('change', listener);
    return () => mq.removeEventListener('change', listener);
  }, [theme]);

  const stateColor: Record<string, string> = {
    disconnected: 'bg-rose-400',
    connecting: 'bg-amber-400 animate-pulse',
    connected: 'bg-emerald-400',
    authenticated: 'bg-emerald-500',
  };

  const stateLabel = intl.formatMessage({ id: `status.${connectionState}` });

  const cycleTheme = () => {
    const next: Theme =
      theme === 'light' ? 'dark' : theme === 'dark' ? 'system' : 'light';
    setTheme(next);
  };

  const ThemeIcon = theme === 'light' ? Sun : theme === 'dark' ? Moon : Monitor;

  return (
    <header className="flex h-14 items-center justify-between border-b border-stone-200 bg-white/80 px-6 backdrop-blur-sm dark:border-stone-800 dark:bg-stone-950/80">
      {/* Connection error banner */}
      {connectionState === 'disconnected' && connectionError && (
        <div className="flex items-center gap-2 text-xs text-rose-600 dark:text-rose-400">
          <span>{intl.formatMessage({ id: 'status.connection_error' }, { msg: connectionError.slice(0, 80) })}</span>
          <button
            onClick={() => reconnect()}
            className="inline-flex items-center gap-1 rounded-md bg-rose-100 px-2 py-1 text-xs font-medium text-rose-700 hover:bg-rose-200 dark:bg-rose-900/30 dark:text-rose-400 dark:hover:bg-rose-900/50"
          >
            <RefreshCw className="h-3 w-3" />
            {intl.formatMessage({ id: 'status.reconnect' })}
          </button>
        </div>
      )}
      {/* Update notification banner */}
      {updateNotification?.available && !updateDismissed && connectionState !== 'disconnected' && (
        <div className="flex items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-1.5 dark:border-amber-800 dark:bg-amber-900/20">
          <ArrowUpCircle className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
          <span className="text-xs text-amber-700 dark:text-amber-400">
            {intl.formatMessage(
              { id: 'update.notification' },
              { version: updateNotification.latest_version }
            )}
          </span>
          <button
            onClick={() => navigate('/settings?tab=update')}
            className="whitespace-nowrap rounded-md bg-amber-500 px-2 py-0.5 text-xs font-medium text-white transition-colors hover:bg-amber-600"
          >
            {intl.formatMessage({ id: 'update.viewDetails' })}
          </button>
          <button
            onClick={dismissUpdate}
            className="rounded p-0.5 text-amber-400 transition-colors hover:bg-amber-100 hover:text-amber-600 dark:hover:bg-amber-900/40"
            title={intl.formatMessage({ id: 'update.dismiss' })}
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {(connectionState !== 'disconnected' || !connectionError) && !(updateNotification?.available && !updateDismissed && connectionState !== 'disconnected') && <div />}

      <div className="flex items-center gap-4">
        {/* Connection Status */}
        <button
          onClick={connectionState === 'disconnected' ? () => reconnect() : undefined}
          className={cn(
            'flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-sm transition-colors',
            connectionState === 'disconnected'
              ? 'text-rose-500 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20 cursor-pointer'
              : 'text-stone-500 dark:text-stone-400 cursor-default'
          )}
        >
          <div className={cn('h-2 w-2 rounded-full', stateColor[connectionState] ?? 'bg-stone-400')} />
          <span>{stateLabel}</span>
        </button>

        {/* Theme Toggle */}
        <button
          onClick={cycleTheme}
          className="rounded-lg p-2 text-stone-500 hover:bg-stone-100 hover:text-stone-700 dark:text-stone-400 dark:hover:bg-stone-800 dark:hover:text-stone-200"
          title={`Theme: ${theme}`}
        >
          <ThemeIcon className="h-4 w-4" />
        </button>
      </div>
    </header>
  );
}
