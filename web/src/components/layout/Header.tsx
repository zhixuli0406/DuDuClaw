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
  Search,
  Menu,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useRef, useState, useCallback } from 'react';
import { ModeToggle } from './ModeToggle';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSidebarStore } from '@/stores/sidebar-store';
import { useUiModeStore } from '@/stores/ui-mode-store';

/** Best-effort platform hint for the ⌘K vs Ctrl+K affordance. */
function isMacLike(): boolean {
  if (typeof navigator === 'undefined') return false;
  const src = `${navigator.platform ?? ''} ${navigator.userAgent ?? ''}`;
  return /Mac|iPhone|iPad|iPod/i.test(src);
}

function CommandTrigger() {
  const intl = useIntl();
  const openPalette = useCommandPaletteStore((s) => s.openPalette);
  const modKey = isMacLike() ? '⌘' : 'Ctrl';
  return (
    <button
      onClick={openPalette}
      aria-label={intl.formatMessage({ id: 'cmdk.title' })}
      title={intl.formatMessage({ id: 'cmdk.title' })}
      className="flex items-center gap-2 rounded-lg border border-stone-300/50 px-2.5 py-1.5 text-sm text-stone-500 transition-colors hover:border-stone-400/60 hover:bg-stone-500/5 hover:text-stone-700 dark:border-white/10 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200"
    >
      <Search className="h-4 w-4 shrink-0" />
      <span className="hidden sm:inline">{intl.formatMessage({ id: 'cmdk.trigger' })}</span>
      <kbd className="hidden items-center gap-0.5 rounded border border-stone-300/60 px-1 py-0.5 text-[10px] font-medium text-stone-400 sm:inline-flex dark:border-white/10">
        {modKey}K
      </kbd>
    </button>
  );
}

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

function MobileMenuButton() {
  const intl = useIntl();
  const mode = useUiModeStore((s) => s.mode);
  const toggleMobile = useSidebarStore((s) => s.toggleMobile);
  // The workspace rail is already slim; only the dashboard sidebar needs a drawer.
  if (mode === 'workspace') return null;
  return (
    <button
      onClick={toggleMobile}
      aria-label={intl.formatMessage({ id: 'header.menu' })}
      title={intl.formatMessage({ id: 'header.menu' })}
      className="-ml-1 rounded-lg p-2 text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 md:hidden dark:text-stone-400 dark:hover:text-stone-200"
    >
      <Menu className="h-5 w-5" />
    </button>
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
  const updateRestarting = useUpdateStore((s) => s.restarting);
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
    <header className="glass-chrome relative z-40 flex h-14 items-center justify-between border-b border-stone-300/40 px-4 md:px-6 dark:border-white/8">
      {/* Mobile nav drawer toggle (hidden at md+) */}
      <MobileMenuButton />

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
      {/* Update restart-in-progress banner */}
      {updateRestarting && (
        <div className="flex items-center gap-2 rounded-lg border border-emerald-300/40 bg-emerald-100/50 px-3 py-1.5 backdrop-blur-sm dark:border-emerald-700/40 dark:bg-emerald-900/25">
          <RefreshCw className="h-4 w-4 shrink-0 animate-spin text-emerald-600 dark:text-emerald-400" />
          <span className="text-xs text-emerald-700 dark:text-emerald-400">
            {intl.formatMessage({ id: 'update.restarting' })}
          </span>
        </div>
      )}
      {/* Update notification banner */}
      {updateNotification?.available && !updateDismissed && !updateRestarting && connectionState !== 'disconnected' && (
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
        {/* Command palette trigger (⌘K) */}
        <CommandTrigger />

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

        {/* Simple ⇄ Advanced shell switch */}
        <ModeToggle />

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
