import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useUpdateStore } from '@/stores/update-store';
import { useAuthStore } from '@/stores/auth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useLocaleStore, localeNames } from '@/i18n';
import {
  Sun,
  Moon,
  Monitor,
  RefreshCw,
  ArrowUpCircle,
  X,
  Languages,
  Check,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useRef, useState, useCallback } from 'react';

function LanguageMenu() {
  const intl = useIntl();
  const locale = useLocaleStore((s) => s.locale);
  const setLocale = useLocaleStore((s) => s.setLocale);
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  return (
    <div ref={menuRef} className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        title={intl.formatMessage({ id: 'header.language' })}
        className="flex items-center gap-1.5 rounded-lg p-2 text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200"
      >
        <Languages className="h-4 w-4" />
        <span className="text-xs font-medium">{localeNames[locale] ?? locale}</span>
      </button>
      {open && (
        <div
          role="menu"
          className="glass-overlay absolute right-0 top-full z-50 mt-2 w-36 overflow-hidden rounded-xl p-1"
        >
          {Object.entries(localeNames).map(([code, label]) => (
            <button
              key={code}
              role="menuitemradio"
              aria-checked={locale === code}
              onClick={() => {
                setLocale(code);
                setOpen(false);
              }}
              className={cn(
                'flex w-full items-center justify-between rounded-lg px-3 py-2 text-sm transition-colors',
                locale === code
                  ? 'font-medium text-amber-600 dark:text-amber-400'
                  : 'text-stone-600 hover:bg-stone-500/10 dark:text-stone-300'
              )}
            >
              <span>{label}</span>
              {locale === code && <Check className="h-3.5 w-3.5" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

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
  const theme = useThemeStore((s) => s.theme);
  const cycleTheme = useThemeStore((s) => s.cycleTheme);

  // Check for updates on mount (in case gateway already has cached info)
  useEffect(() => {
    if (connectionState === 'authenticated') {
      checkUpdate();
    }
  }, [connectionState, checkUpdate]);

  const stateColor: Record<string, string> = {
    disconnected: 'bg-rose-400',
    connecting: 'bg-amber-400 animate-pulse',
    connected: 'bg-emerald-400',
    authenticated: 'bg-emerald-500',
  };

  const stateLabel = intl.formatMessage({ id: `status.${connectionState}` });

  const ThemeIcon = theme === 'light' ? Sun : theme === 'dark' ? Moon : Monitor;

  return (
    <header className="glass-chrome relative z-40 flex h-14 items-center justify-between border-b border-stone-300/40 px-6 dark:border-white/8">
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
        <div className="flex items-center gap-2 rounded-lg border border-amber-300/40 bg-amber-100/50 px-3 py-1.5 backdrop-blur-sm dark:border-amber-700/40 dark:bg-amber-900/25">
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

      <div className="flex items-center gap-2">
        {/* Connection Status */}
        <button
          onClick={connectionState === 'disconnected' ? () => reconnect() : undefined}
          className={cn(
            'flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-sm transition-colors',
            connectionState === 'disconnected'
              ? 'text-rose-500 hover:bg-rose-500/10 dark:text-rose-400 cursor-pointer'
              : 'text-stone-500 dark:text-stone-400 cursor-default'
          )}
        >
          <div className={cn('h-2 w-2 rounded-full', stateColor[connectionState] ?? 'bg-stone-400')} />
          <span>{stateLabel}</span>
        </button>

        {/* Language Switcher */}
        <LanguageMenu />

        {/* Theme Toggle */}
        <button
          onClick={cycleTheme}
          title={intl.formatMessage({ id: 'header.theme' }, { theme })}
          className="rounded-lg p-2 text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200"
        >
          <ThemeIcon className="h-4 w-4" />
        </button>
      </div>
    </header>
  );
}
