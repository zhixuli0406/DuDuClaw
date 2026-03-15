import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { Sun, Moon, Monitor, RefreshCw } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useState } from 'react';

type Theme = 'light' | 'dark' | 'system';

export function Header() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const connectionError = useConnectionStore((s) => s.error);
  const reconnect = useConnectionStore((s) => s.connect);
  const [theme, setTheme] = useState<Theme>('system');

  useEffect(() => {
    const stored = localStorage.getItem('duduclaw-theme') as Theme | null;
    if (stored) setTheme(stored);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
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
          <span>連線失敗: {connectionError.slice(0, 80)}</span>
          <button
            onClick={() => reconnect()}
            className="inline-flex items-center gap-1 rounded-md bg-rose-100 px-2 py-1 text-xs font-medium text-rose-700 hover:bg-rose-200 dark:bg-rose-900/30 dark:text-rose-400 dark:hover:bg-rose-900/50"
          >
            <RefreshCw className="h-3 w-3" />
            重連
          </button>
        </div>
      )}
      {(connectionState !== 'disconnected' || !connectionError) && <div />}

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
