import { useIntl } from 'react-intl';
import { useNavigate, useLocation, NavLink } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useUpdateStore } from '@/stores/update-store';
import { useAuthStore } from '@/stores/auth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useApprovalsStore } from '@/stores/approvals-store';
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
  Trophy,
  Bell,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useEffect, useMemo, useRef, useState, useCallback } from 'react';
import { Breadcrumbs } from './Breadcrumbs';
import { crumbsFor } from './nav-model';
import { CoinChip } from '@/components/ui';
import { useGrowthStore } from '@/stores/growth-store';
import { useTodayCost } from '@/components/growth/useTodayCost';
import { hasMinRole } from '@/lib/roles';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSidebarStore } from '@/stores/sidebar-store';

/** Notification bell — the unified "needs me" count → quick-jump to the inbox. */
function BellTrigger() {
  const intl = useIntl();
  const count = useApprovalsStore((s) => s.pendingCount);
  return (
    <NavLink
      to="/inbox"
      aria-label={intl.formatMessage({ id: 'nav.inbox' })}
      title={intl.formatMessage({ id: 'nav.inbox' })}
      className={({ isActive }) =>
        cn(
          'relative rounded-lg p-2 transition-colors',
          isActive
            ? 'text-amber-600 dark:text-amber-400'
            : 'text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200',
        )
      }
    >
      <Bell className="h-4 w-4" />
      {count > 0 && (
        <span className="absolute -right-0.5 -top-0.5 inline-flex min-w-[1rem] items-center justify-center rounded-full bg-rose-500 px-1 text-[9px] font-semibold tabular-nums leading-none text-white">
          {count > 99 ? '99+' : count}
        </span>
      )}
    </NavLink>
  );
}

/**
 * HUD spend chip (§4.3 / T10.5). Today's spend from `growth.daily_report`
 * ({date: today}), which returns a live rolling figure — labelled "今日" (the
 * value is still updating). On error it degrades to the cumulative account
 * total, labelled "累計", never a misleading $0. Manager+ only: cost is not
 * shown to employees (the daily-report RPC isn't role-gated server-side, so the
 * gate is enforced here — `enabled: false` skips the fetch entirely).
 */
function HudCostChip() {
  const intl = useIntl();
  const navigate = useNavigate();
  const role = useAuthStore((s) => s.user?.role);
  const canSee = hasMinRole(role, 'manager');
  const { cents, mode } = useTodayCost({ enabled: canSee });

  if (!canSee) return null;
  if (mode === 'loading' || cents === null) {
    return <span className="hidden h-7 w-16 animate-pulse rounded-full bg-stone-500/10 sm:block dark:bg-white/5" aria-hidden="true" />;
  }
  const label = intl.formatMessage({ id: mode === 'today' ? 'hud.cost.today' : 'hud.cost.cumulative' });
  return (
    <span className="hidden sm:inline-flex">
      <CoinChip
        cents={cents}
        onClick={() => navigate('/manage/billing')}
        title={`${label} · ${intl.formatMessage({ id: 'nav.billing' })}`}
      />
    </span>
  );
}

/**
 * HUD XP capsule (§4.3 / T10.5) — Lv + within-level progress from the growth
 * snapshot (polled shell-wide by GrowthMount). Plays a badge-pop when the
 * company levels up (store `levelUpNonce`, T10.4). Falls back to a placeholder
 * until the first snapshot lands.
 */
function HudXpChip() {
  const intl = useIntl();
  const navigate = useNavigate();
  const snapshot = useGrowthStore((s) => s.snapshot);
  const levelUpNonce = useGrowthStore((s) => s.levelUpNonce);
  const [pop, setPop] = useState(false);
  const seenNonce = useRef(levelUpNonce);

  useEffect(() => {
    if (levelUpNonce === seenNonce.current) return;
    seenNonce.current = levelUpNonce;
    setPop(true);
    const t = window.setTimeout(() => setPop(false), 900);
    return () => window.clearTimeout(t);
  }, [levelUpNonce]);

  const span = snapshot ? snapshot.xp_into_level + snapshot.xp_for_next_level : 0;
  const pct = snapshot && span > 0 ? Math.round((snapshot.xp_into_level / span) * 100) : 0;

  return (
    <button
      onClick={() => navigate('/growth')}
      title={intl.formatMessage({ id: 'hud.growth' })}
      aria-label={intl.formatMessage({ id: 'hud.growth' })}
      className={cn(
        'hidden items-center gap-1.5 rounded-full bg-[color:var(--xp)]/10 px-2.5 py-1 ring-1 ring-inset ring-[color:var(--xp)]/25 transition-colors hover:bg-[color:var(--xp)]/15 sm:inline-flex',
        pop && 'animate-badge-pop',
      )}
    >
      <Trophy className="h-4 w-4 text-[color:var(--xp)]" aria-hidden="true" />
      <span className="text-xs font-semibold tabular-nums text-stone-700 dark:text-stone-200">
        {snapshot ? `Lv.${snapshot.level}` : intl.formatMessage({ id: 'sidebar.level.placeholder' })}
      </span>
      {snapshot && (
        <span className="hidden h-1.5 w-10 overflow-hidden rounded-full bg-stone-500/15 md:block dark:bg-white/10" aria-hidden="true">
          <span className="block h-full rounded-full bg-[color:var(--xp)]" style={{ width: `${pct}%` }} />
        </span>
      )}
    </button>
  );
}

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
  const toggleMobile = useSidebarStore((s) => s.toggleMobile);
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
  const location = useLocation();
  const crumbs = useMemo(
    () => crumbsFor(location.pathname).map((c) => ({ label: intl.formatMessage({ id: c.labelId }), to: c.to })),
    [location.pathname, intl],
  );
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

      {/* Breadcrumb trail (paperclip P6) — hidden while a banner claims the row */}
      {crumbs.length > 0 && !(connectionState === 'disconnected' && connectionError) && (
        <Breadcrumbs items={crumbs} className="min-w-0 flex-1" />
      )}

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
        {/* HUD: today spend + XP capsule (§4.3) */}
        <HudCostChip />
        <HudXpChip />

        {/* Notification bell — needs-me inbox quick-jump */}
        <BellTrigger />

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
