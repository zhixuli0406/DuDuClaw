import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { Sun, Moon, Monitor } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useState } from 'react';

type Theme = 'light' | 'dark' | 'system';

export function Header() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
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
    disconnected: 'bg-stone-400',
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
      <div />
      <div className="flex items-center gap-4">
        {/* Connection Status */}
        <div className="flex items-center gap-2 text-sm text-stone-500 dark:text-stone-400">
          <div
            className={cn(
              'h-2 w-2 rounded-full',
              stateColor[connectionState] ?? 'bg-stone-400'
            )}
          />
          <span>{stateLabel}</span>
        </div>

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
