import { useState } from 'react';
import { NavLink } from 'react-router';
import { useIntl } from 'react-intl';
import { ChevronDown, LogOut } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { navGroups, type NavItem } from './nav-model';

const COLLAPSE_KEY = 'duduclaw-nav-collapsed';

function loadCollapsed(): Record<string, boolean> {
  try {
    return JSON.parse(localStorage.getItem(COLLAPSE_KEY) ?? '{}');
  } catch {
    return {};
  }
}

export function Sidebar() {
  const intl = useIntl();
  const status = useSystemStore((s) => s.status);
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(loadCollapsed);

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

  const visibleGroups = navGroups
    .map((group) => ({
      ...group,
      items: group.items.filter((item: NavItem) => hasMinRole(user?.role, item.minRole)),
    }))
    .filter((group) => group.items.length > 0);

  return (
    <aside className="glass-chrome relative z-40 flex w-60 flex-col border-r border-stone-300/40 dark:border-white/8">
      {/* Logo */}
      <div className="flex items-center gap-3 px-5 py-5">
        <span
          className="grid h-9 w-9 place-items-center rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 text-lg shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
          role="img"
          aria-label="paw"
        >
          🐾
        </span>
        <div className="min-w-0">
          <h1 className="truncate text-base font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="truncate text-xs text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'app.subtitle' })}
          </p>
        </div>
      </div>

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
        <p className="font-mono text-[11px] tracking-wide text-stone-400 dark:text-stone-500">
          {status?.version ?? 'v0.12.0'}
        </p>
      </div>
    </aside>
  );
}
