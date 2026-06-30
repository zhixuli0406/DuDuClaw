import { useState } from 'react';
import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import {
  ChevronDown,
  LogOut,
  Home,
  MessageCircle,
  Puzzle,
  KanbanSquare,
  BookOpen,
  LayoutGrid,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { hasMinRole } from '@/lib/roles';
import { navGroups, type NavItem } from './nav-model';
import { EditionBadge } from './EditionBadge';

const COLLAPSE_KEY = 'duduclaw-nav-collapsed';

function loadCollapsed(): Record<string, boolean> {
  try {
    return JSON.parse(localStorage.getItem(COLLAPSE_KEY) ?? '{}');
  } catch {
    return {};
  }
}

function Logo({ compact }: { compact?: boolean }) {
  const intl = useIntl();
  return (
    <div className={cn('flex items-center gap-3 px-5 py-5', compact && 'justify-center px-0')}>
      <span
        className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 text-lg shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
        role="img"
        aria-label="paw"
      >
        🐾
      </span>
      {!compact && (
        <div className="min-w-0">
          <h1 className="truncate text-base font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="truncate text-xs text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'app.subtitle' })}
          </p>
        </div>
      )}
    </div>
  );
}

/**
 * Narrow icon rail shown in workspace mode (TODO-genspark-workspace-shell §P5.2)
 * — the Genspark-style slim nav. A curated subset of existing routes; the last
 * item flips back to the full dashboard. Role-gating still applies.
 */
function WorkspaceRail() {
  const intl = useIntl();
  const navigate = useNavigate();
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  const setMode = useUiModeStore((s) => s.setMode);

  const items: ReadonlyArray<{ to: string; icon: typeof Home; label: string }> = [
    { to: '/workspace', icon: Home, label: 'nav.home' },
    { to: '/webchat', icon: MessageCircle, label: 'launcher.claw.label' },
    { to: '/skills', icon: Puzzle, label: 'nav.skills' },
    { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks' },
    { to: '/wiki', icon: BookOpen, label: 'nav.wiki' },
  ];

  const goDashboard = () => {
    setMode('dashboard');
    navigate('/');
  };

  return (
    <aside className="glass-chrome relative z-40 flex w-16 flex-col items-center border-r border-stone-300/40 dark:border-white/8">
      <Logo compact />
      <nav className="flex flex-1 flex-col items-center gap-1 pt-2">
        {items.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            title={intl.formatMessage({ id: label })}
            className={({ isActive }) =>
              cn(
                'grid h-10 w-10 place-items-center rounded-xl transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                isActive
                  ? 'bg-amber-500/12 text-amber-700 ring-1 ring-inset ring-amber-500/25 dark:bg-amber-400/10 dark:text-amber-300'
                  : 'text-stone-500 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200'
              )
            }
          >
            <Icon className="h-[1.125rem] w-[1.125rem]" />
            <span className="sr-only">{intl.formatMessage({ id: label })}</span>
          </NavLink>
        ))}

        <button
          onClick={goDashboard}
          title={intl.formatMessage({ id: 'mode.dashboard' })}
          aria-label={intl.formatMessage({ id: 'mode.dashboard' })}
          className="mt-1 grid h-10 w-10 place-items-center rounded-xl text-stone-500 transition-colors hover:bg-stone-500/8 hover:text-stone-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200"
        >
          <LayoutGrid className="h-[1.125rem] w-[1.125rem]" />
        </button>
      </nav>

      {user && (
        <button
          onClick={logout}
          title={intl.formatMessage({ id: 'auth.logout' })}
          aria-label={intl.formatMessage({ id: 'auth.logout' })}
          className="mb-3 grid h-10 w-10 place-items-center rounded-xl text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
        >
          <LogOut className="h-4 w-4" />
        </button>
      )}
    </aside>
  );
}

export function Sidebar() {
  const intl = useIntl();
  const status = useSystemStore((s) => s.status);
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  const mode = useUiModeStore((s) => s.mode);
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(loadCollapsed);

  if (mode === 'workspace') return <WorkspaceRail />;

  const toggle = (label: string) => {
    setCollapsed((prev) => {
      const next = { ...prev, [label]: !prev[label] };
      try {
        localStorage.setItem(COLLAPSE_KEY, JSON.stringify(next));
      } catch {
        /* ignore quota / private-mode failures */
      }
      return next;
    });
  };

  // Personal edition hides enterprise-only management surfaces. An absent
  // `edition_profile` (older gateway) is treated as enterprise → show all.
  const isPersonal = status?.edition_profile === 'personal';

  const visibleGroups = navGroups
    .map((group) => ({
      ...group,
      items: group.items.filter(
        (item: NavItem) =>
          hasMinRole(user?.role, item.minRole) && !(isPersonal && item.enterprise)
      ),
    }))
    .filter((group) => group.items.length > 0);

  return (
    <aside className="glass-chrome relative z-40 flex w-60 flex-col border-r border-stone-300/40 dark:border-white/8">
      {/* Logo */}
      <Logo />

      {/* Grouped navigation */}
      <nav className="flex-1 space-y-1 overflow-y-auto px-3 pb-3">
        {visibleGroups.map((group) => {
          const isCollapsed = !!collapsed[group.label];
          return (
            <div key={group.label} className="pt-2">
              <button
                onClick={() => toggle(group.label)}
                aria-expanded={!isCollapsed}
                className="group flex w-full items-center justify-between rounded-md px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-stone-400 transition-colors hover:text-stone-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-500 dark:hover:text-stone-300"
              >
                <span>{intl.formatMessage({ id: group.label })}</span>
                <ChevronDown
                  className={cn(
                    'h-3.5 w-3.5 transition-transform',
                    isCollapsed && '-rotate-90'
                  )}
                />
              </button>
              {!isCollapsed && (
                <div className="mt-0.5 space-y-0.5">
                  {group.items.map(({ to, icon: Icon, label }) => (
                    <NavLink
                      key={to}
                      to={to}
                      end={to === '/'}
                      data-tour={`nav:${to}`}
                      className={({ isActive }) =>
                        cn(
                          'group relative flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                          isActive
                            ? 'bg-amber-500/12 text-amber-700 ring-1 ring-inset ring-amber-500/25 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/20'
                            : 'text-stone-600 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200'
                        )
                      }
                    >
                      {({ isActive }) => (
                        <>
                          <span
                            className={cn(
                              'absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-amber-500 transition-opacity dark:bg-amber-400',
                              isActive ? 'opacity-100' : 'opacity-0'
                            )}
                            aria-hidden="true"
                          />
                          <Icon className="h-[1.125rem] w-[1.125rem] shrink-0" />
                          <span className="truncate">{intl.formatMessage({ id: label })}</span>
                        </>
                      )}
                    </NavLink>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </nav>

      {/* User Info + Footer */}
      <div className="border-t border-stone-300/40 px-4 py-3 dark:border-white/8">
        {user && (
          <div className="mb-2 flex items-center justify-between">
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-stone-700 dark:text-stone-300">
                {user.display_name}
              </p>
              <p className="truncate text-xs text-stone-400 dark:text-stone-500">{user.role}</p>
            </div>
            <button
              onClick={logout}
              className="rounded p-1.5 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
              title={intl.formatMessage({ id: 'auth.logout' })}
            >
              <LogOut className="h-4 w-4" />
            </button>
          </div>
        )}
        <div className="flex items-center justify-between gap-2">
          <p className="font-mono text-[11px] tracking-wide text-stone-400 dark:text-stone-500">
            {status?.version ?? 'v0.12.0'}
          </p>
          <EditionBadge />
        </div>
      </div>
    </aside>
  );
}
