import { NavLink, Navigate, Outlet, useLocation } from 'react-router';
import { useIntl } from 'react-intl';
import { SettingsIcon } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { cn } from '@/lib/utils';
import { PageHeader } from '@/components/mds';
import { manageNav, manageAdvancedNav, type NavItem } from './nav-model';

/** Row styling shared by the five primary entries and the 進階設定 sub-list. */
const railLink = (isActive: boolean, indent: boolean) =>
  cn(
    'flex h-8 shrink-0 items-center gap-2 rounded-md px-2.5 text-sm whitespace-nowrap outline-none transition-colors',
    'focus-visible:ring-2 focus-visible:ring-ring/50',
    indent && 'md:ml-3',
    isActive
      ? 'bg-surface-selected font-medium text-foreground'
      : 'text-muted-foreground hover:bg-surface-hover hover:text-foreground',
  );

/** One rail entry. `forceActive` lets 進階設定 stay lit inside its subtree. */
function RailRow({
  item,
  indent = false,
  forceActive = false,
}: {
  item: NavItem;
  indent?: boolean;
  forceActive?: boolean;
}) {
  const intl = useIntl();
  return (
    <NavLink
      to={item.to}
      data-tour={`nav:${item.to}`}
      className={({ isActive }) => railLink(isActive || forceActive, indent)}
    >
      <item.icon className="size-4 shrink-0" />
      <span className="truncate">{intl.formatMessage({ id: item.label })}</span>
    </NavLink>
  );
}

/**
 * ManageShell (§6.1, paperclip P3 → WP4.1 Multica Settings-式 rework) — the
 * single Zone D entry. A left rail, vertical ≥md / horizontal-scroll on mobile,
 * drives real nested routes (NavLink, not `?tab=` — manage pages are
 * bookmarkable) rendering the active management page via <Outlet>.
 *
 * 2026-08-04 (D18) the rail collapsed from three labelled groups to five plain
 * rows — 通道管理 / 整合 / 帳戶與登入 / 系統更新 / 進階設定 — because the client
 * could not find the two things she actually needed among fifteen. Nothing was
 * removed: the other eleven surfaces keep their routes and unfold as an
 * indented sub-list under 進階設定 whenever the viewer is inside that subtree.
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
 * `MemoryPage`) — doing so would strip the child pages' only source of
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
  const advanced = filterVisible(manageAdvancedNav, role, isPersonal);

  // Bare /manage → land on the first surface the viewer can actually see.
  if (location.pathname === '/manage' || location.pathname === '/manage/') {
    const first = visible[0] ?? advanced[0];
    return first ? <Navigate to={first.to} replace /> : <Navigate to="/" replace />;
  }

  // 進階設定 holds everything that isn't one of the four promoted surfaces
  // (2026-08-04, D18). Its sub-list unfolds only while the viewer is inside the
  // subtree — the rail stays five rows tall the rest of the time.
  // Matched against the UNGATED list on purpose: a viewer who deep-links to a
  // surface their edition hides from the rail (e.g. Personal → /manage/security)
  // should still see 進階設定 lit rather than a rail that claims they are
  // nowhere.
  const inAdvanced =
    location.pathname.startsWith('/manage/advanced') ||
    manageAdvancedNav.some((i) => location.pathname.startsWith(i.to));

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
          <div className="flex shrink-0 gap-1 md:flex md:w-full md:flex-col">
            {visible.map((item) => (
              <RailRow
                key={item.to}
                item={item}
                // 進階設定 stays highlighted while you are anywhere inside it.
                forceActive={item.to === '/manage/advanced' && inAdvanced}
              />
            ))}
            {inAdvanced &&
              advanced.map((item) => <RailRow key={item.to} item={item} indent />)}
          </div>
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
