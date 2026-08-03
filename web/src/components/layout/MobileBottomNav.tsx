import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Plus } from 'lucide-react';
import { useSystemStore } from '@/stores/system-store';
import { cn } from '@/lib/utils';
import { mobileNavItems, type NavItem } from './nav-model';

/**
 * MobileBottomNav — Zone A quick access on small screens (§4.3). Slots:
 * 儀表板 / 對話 / ＋交辦（center raised action, links to the task board's create
 * intent） / 對話紀錄 / 任務. Hidden at md+ (the sidebar takes over). Two balanced
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
  const navigate = useNavigate();
  // Personal IA (2026-07-29 client feedback round 2): the centre ＋交辦 quick
  // action is task-board chrome — hidden on Personal like the sidebar button.
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';

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
          centred by the equal-flex groups on either side. */}
      {!isPersonal && (
        <div className="flex w-16 shrink-0 items-center justify-center">
          <button
            type="button"
            // TODO(v2-V5): route to the task board's create intent until a global
            // create-task modal exists.
            onClick={() => navigate('/tasks?new=1')}
            aria-label={intl.formatMessage({ id: 'sidebar.newTask' })}
            title={intl.formatMessage({ id: 'sidebar.newTask' })}
            className="-mt-6 grid size-14 place-items-center rounded-full bg-brand text-brand-foreground shadow-[var(--menu-shadow)] ring-4 ring-sidebar transition-transform active:translate-y-px active:scale-95"
          >
            <Plus className="size-6" />
          </button>
        </div>
      )}

      <div className="flex flex-1 items-stretch">
        {right.map((item) => (
          <BottomNavLink key={item.to} item={item} />
        ))}
      </div>
    </nav>
  );
}
