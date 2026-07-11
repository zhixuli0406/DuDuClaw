import { NavLink, Navigate, Outlet, useLocation } from 'react-router';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { cn } from '@/lib/utils';
import { manageNav } from './nav-model';

/**
 * ManageShell (§6.1, paperclip P3) — the single Zone D entry. Collapses the
 * former 17-item admin navigation wall into one shell: a left subnav tree
 * (filtered to what the viewer may see) + a Breadcrumb'd content area rendering
 * the active management page via <Outlet>.
 *
 * Gating is defence-in-depth: the whole shell needs `manager`+ (employees are
 * redirected home), and each subnav item re-gates by its own `minRole` /
 * `enterprise`. Front-end only — the gateway RPC layer is the real gate (WP11).
 */
export function ManageShell() {
  const intl = useIntl();
  const location = useLocation();
  const role = useAuthStore((s) => s.user?.role);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';

  // Fail-closed at the shell boundary: no manager+ → never render management.
  if (!hasMinRole(role, 'manager')) return <Navigate to="/" replace />;

  const visible = filterVisible(manageNav, role, isPersonal);

  // Bare /manage → land on the first surface the viewer can actually see.
  if (location.pathname === '/manage' || location.pathname === '/manage/') {
    return visible.length > 0 ? <Navigate to={visible[0].to} replace /> : <Navigate to="/" replace />;
  }

  return (
    <div className="flex flex-col gap-4 lg:flex-row">
      <aside className="glass-chrome shrink-0 rounded-xl border border-stone-300/40 p-2 lg:w-56 dark:border-white/8">
        <p className="px-3 py-2 text-[11px] font-semibold uppercase tracking-wider text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'nav.manage' })}
        </p>
        <nav className="flex gap-1 overflow-x-auto lg:flex-col lg:overflow-visible">
          {visible.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                cn(
                  'flex shrink-0 items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                  isActive
                    ? 'bg-amber-500/12 text-amber-700 ring-1 ring-inset ring-amber-500/25 dark:bg-amber-400/10 dark:text-amber-300'
                    : 'text-stone-600 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200',
                )
              }
            >
              <Icon className="h-[1.125rem] w-[1.125rem] shrink-0" />
              <span className="truncate">{intl.formatMessage({ id: label })}</span>
            </NavLink>
          ))}
        </nav>
      </aside>

      <div className="min-w-0 flex-1">
        <Outlet />
      </div>
    </div>
  );
}
