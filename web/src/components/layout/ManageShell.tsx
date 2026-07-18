import { NavLink, Navigate, Outlet, useLocation } from 'react-router';
import { useIntl } from 'react-intl';
import { SettingsIcon } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { cn } from '@/lib/utils';
import { PageHeader } from '@/components/mds';
import { manageNav, type NavItem } from './nav-model';

/**
 * Rail grouping for the Settings-style shell (spec §5.3 式3, WP4.1). Order
 * mirrors declaration order in `manageNav`; a path missing from the viewer's
 * role/edition-filtered set is simply absent from its group, and a group left
 * with zero visible items disappears entirely (no dangling empty label).
 */
const MANAGE_GROUPS: ReadonlyArray<{ labelId: string; paths: readonly string[] }> = [
  {
    // 營運 — day-to-day operational surfaces.
    labelId: 'manageGroup.operations',
    paths: ['/manage/channels', '/manage/integrations', '/manage/inference', '/manage/system'],
  },
  {
    // 帳務與授權 — money and licensing.
    labelId: 'manageGroup.billing',
    paths: ['/manage/billing', '/manage/license', '/manage/distributors', '/manage/migrate'],
  },
  {
    // 治理 — people, policy, and audit surfaces.
    labelId: 'manageGroup.governance',
    paths: [
      '/manage/users',
      '/manage/departments',
      '/manage/governance',
      '/manage/security',
      '/manage/reliability',
      '/manage/logs',
    ],
  },
];

/**
 * ManageShell (§6.1, paperclip P3 → WP4.1 Multica Settings-式 rework) — the
 * single Zone D entry. A grouped left rail (spec §5.3 式3: 營運 / 帳務與授權 /
 * 治理), vertical ≥md / horizontal-scroll on mobile, drives real nested routes
 * (NavLink, not `?tab=` — manage pages are bookmarkable) rendering the active
 * management page via <Outlet>.
 *
 * Gating is defence-in-depth: the whole shell needs `manager`+ (employees are
 * redirected home), and each subnav item re-gates by its own `minRole` /
 * `enterprise`. Front-end only — the gateway RPC layer is the real gate (WP11).
 *
 * Child management pages are NOT restyled by this WP (that's WP4.2-4.4) — they
 * keep rendering inside their own `Page` container, which carries no padding of
 * its own and relies on the ambient `p-4 md:p-6` the app shell already applies
 * around the route outlet. This shell therefore deliberately does NOT escape
 * that ambient padding (unlike full-bleed shells such as `EditAgentPage` /
 * `KnowledgeShell`) — doing so would strip the child pages' only source of
 * breathing room. The rail + content row simply sits inside it, same as the
 * pre-WP4.1 shell did.
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

  const visibleByPath = new Map(visible.map((item) => [item.to, item]));
  const groups = MANAGE_GROUPS.map((group) => ({
    labelId: group.labelId,
    items: group.paths
      .map((path) => visibleByPath.get(path))
      .filter((item): item is NavItem => item !== undefined),
  })).filter((group) => group.items.length > 0);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PageHeader hideTrigger className="mb-4">
        <SettingsIcon className="size-4 text-muted-foreground" />
        <span className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'nav.manage' })}
        </span>
      </PageHeader>

      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <nav
          aria-label={intl.formatMessage({ id: 'nav.manage' })}
          className="flex shrink-0 gap-1 overflow-x-auto border-b border-surface-border p-2 md:w-56 md:flex-col md:overflow-x-visible md:border-r md:border-b-0 md:p-4"
        >
          {groups.map((group) => (
            <div key={group.labelId} className="flex shrink-0 gap-1 md:flex md:w-full md:flex-col">
              <div className="hidden h-8 items-center px-2 text-xs font-medium text-muted-foreground md:flex">
                {intl.formatMessage({ id: group.labelId })}
              </div>
              {group.items.map((item) => (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) =>
                    cn(
                      'flex h-8 shrink-0 items-center gap-2 rounded-md px-2.5 text-sm whitespace-nowrap outline-none transition-colors',
                      'focus-visible:ring-2 focus-visible:ring-ring/50',
                      isActive
                        ? 'bg-surface-selected font-medium text-foreground'
                        : 'text-muted-foreground hover:bg-surface-hover hover:text-foreground',
                    )
                  }
                >
                  <item.icon className="size-4 shrink-0" />
                  <span className="truncate">{intl.formatMessage({ id: item.label })}</span>
                </NavLink>
              ))}
            </div>
          ))}
        </nav>

        {/* pt-4/pl-6 keep the pane comfortably clear of the rail divider (mobile
            top bar / desktop border-r) instead of butting against it. */}
        <div className="min-w-0 flex-1 pt-4 md:overflow-y-auto md:pt-0 md:pl-6">
          <Outlet />
        </div>
      </div>
    </div>
  );
}
