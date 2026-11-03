import { TASK_ENTRY_POINTS_HIDDEN, chatEntryLabel } from '@/lib/task-entry-visibility';
import { NavLink, useLocation } from 'react-router';
import { useIntl } from 'react-intl';
import { Plus } from 'lucide-react';
import { useOpenAssign } from '@/hooks/useOpenAssign';
import { ASSIGN_PATH } from '@/lib/assign-entry';
import { cn } from '@/lib/utils';
import { mobileNavItemsForRole, type NavItem } from './nav-model';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

/** Mobile quick access with a centred new-conversation action. */
function BottomNavLink({ item }: { item: NavItem }) {
  const intl = useIntl();
  const Icon = item.icon;
  return (
    <NavLink
      to={item.to}
      end={item.to === '/'}
      className={({ isActive }) =>
        cn(
          'relative flex flex-1 flex-col items-center justify-center gap-0.5 text-xs font-medium transition-colors',
          isActive ? 'text-foreground' : 'text-muted-foreground hover:text-foreground',
        )
      }
    >
      <Icon className="size-5" />
      <span className="truncate">{intl.formatMessage({ id: item.label })}</span>
    </NavLink>
  );
}

export function MobileBottomNav() {
  const intl = useIntl();
  const openAssign = useOpenAssign();
  const pathname = useLocation().pathname;
  const onAssign = !TASK_ENTRY_POINTS_HIDDEN && pathname === ASSIGN_PATH;
  const role = useAuthStore((s) => s.user?.role);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const employeeTasks = useSystemStore((s) => s.status?.employee_tasks) === true;
  const mobileNavItems = mobileNavItemsForRole(role, isPersonal, employeeTasks);

  // Split the side items around the center new-conversation action. The center is a
  // fixed-width slot flanked by two equal-flex groups, so the raised ＋ button
  // stays dead-centre horizontally. With 4 items the split is a balanced 2/2,
  // giving every tab the same width.
  const mid = Math.ceil(mobileNavItems.length / 2);
  const left = mobileNavItems.slice(0, mid);
  const right = mobileNavItems.slice(mid);

  return (
    <nav
      aria-label={intl.formatMessage({ id: 'nav.mobile.label' })}
      className="fixed inset-x-0 bottom-0 z-40 flex h-14 items-stretch border-t border-sidebar-border bg-sidebar md:hidden"
    >
      <div className="flex flex-1 items-stretch">
        {left.map((item) => (
          <BottomNavLink key={item.to} item={item} />
        ))}
      </div>

      {/* Equal-flex side groups keep the new-conversation button centred. */}
      <div className="flex w-16 shrink-0 items-center justify-center">
        <button
          type="button"
          onClick={() => openAssign()}
          aria-current={onAssign ? 'page' : undefined}
          aria-label={intl.formatMessage({ id: chatEntryLabel('sidebar.newTask') })}
          title={intl.formatMessage({ id: chatEntryLabel('sidebar.newTask') })}
          className="-mt-6 grid size-14 place-items-center rounded-full bg-brand text-brand-foreground shadow-[var(--menu-shadow)] ring-4 ring-sidebar transition-transform active:translate-y-px active:scale-95"
        >
          <Plus className="size-6" />
        </button>
      </div>

      <div className="flex flex-1 items-stretch">
        {right.map((item) => (
          <BottomNavLink key={item.to} item={item} />
        ))}
      </div>
    </nav>
  );
}
