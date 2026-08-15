import { NavLink } from 'react-router';
import { useIntl } from 'react-intl';
import { Plus } from 'lucide-react';
import { useAssignStore } from '@/stores/assign-store';
import { cn } from '@/lib/utils';
import { mobileNavItems, type NavItem } from './nav-model';

/**
 * MobileBottomNav — Zone A quick access on small screens (§4.3). Slots:
 * 儀表板 / 對話 / ＋交辦（center raised action, opens the AssignSheet） /
 * 對話紀錄 / 任務. Hidden at md+ (the sidebar takes over). Two balanced
 * side groups (2 left / 2 right) flank the centre ＋交辦; the ＋ remains the quick
 * create entry.
 *
 * 2026-08-04 (D17): 收件匣 left this bar, taking the only badge with it. The
 * pending count now shows as a bell in the mobile top bar (`MainLayout`) that
 * appears only when something is actually waiting.
 */
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
  // UX plan I-1a: 交辦 is the primary action of both editions, so the centre ＋
  // is no longer hidden on Personal (it was collateral damage of the
  // 2026-07-29 "hide task-board chrome" round).
  const openAssign = useAssignStore((s) => s.openAssign);

  // Split the side items around the center ＋交辦 action. The center is a
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

      {/* Center raised action: ＋交辦 — a fixed-width slot kept horizontally
          centred by the equal-flex groups on either side. Opens the one
          AssignSheet (UX plan I-1a); it used to route to the task board's
          `?new=1` create-a-card intent, which the code itself flagged as a
          placeholder and which never started the autonomous loop. */}
      <div className="flex w-16 shrink-0 items-center justify-center">
        <button
          type="button"
          onClick={() => openAssign()}
          aria-label={intl.formatMessage({ id: 'sidebar.newTask' })}
          title={intl.formatMessage({ id: 'sidebar.newTask' })}
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
