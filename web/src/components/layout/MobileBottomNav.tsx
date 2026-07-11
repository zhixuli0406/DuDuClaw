import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Plus } from 'lucide-react';
import { useApprovalsStore } from '@/stores/approvals-store';
import { cn } from '@/lib/utils';
import { mobileNavItems, type NavItem } from './nav-model';

/**
 * MobileBottomNav — Zone A quick access on small screens (§4.3). Five slots:
 * 首頁 / 收件匣 / ＋交辦（center raised action） / 任務 / 對話. Hidden at md+ (the
 * sidebar takes over). The inbox slot carries the live "needs me" count.
 */
function BottomNavLink({ item, inboxCount }: { item: NavItem; inboxCount: number }) {
  const intl = useIntl();
  const Icon = item.icon;
  return (
    <NavLink
      to={item.to}
      end={item.to === '/'}
      className={({ isActive }) =>
        cn(
          'relative flex flex-1 flex-col items-center justify-center gap-0.5 text-[10px] font-medium transition-colors',
          isActive
            ? 'text-amber-600 dark:text-amber-400'
            : 'text-stone-500 hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200',
        )
      }
    >
      <span className="relative">
        <Icon className="h-5 w-5" />
        {item.badge === 'inbox' && inboxCount > 0 && (
          <span
            className="absolute -right-2 -top-1.5 inline-flex min-w-[1rem] items-center justify-center rounded-full bg-rose-500 px-1 text-[9px] font-semibold tabular-nums leading-none text-white"
            aria-label={intl.formatMessage({ id: 'nav.inbox.pending' }, { count: inboxCount })}
          >
            {inboxCount > 99 ? '99+' : inboxCount}
          </span>
        )}
      </span>
      <span className="truncate">{intl.formatMessage({ id: item.label })}</span>
    </NavLink>
  );
}

export function MobileBottomNav() {
  const intl = useIntl();
  const navigate = useNavigate();
  const inboxCount = useApprovalsStore((s) => s.pendingCount);

  // Split the four side items 2 | center | 2.
  const left = mobileNavItems.slice(0, 2);
  const right = mobileNavItems.slice(2, 4);

  return (
    <nav
      aria-label={intl.formatMessage({ id: 'nav.mobile.label' })}
      className="glass-chrome fixed inset-x-0 bottom-0 z-40 flex h-14 items-stretch border-t border-stone-300/40 md:hidden dark:border-white/8"
    >
      {left.map((item) => (
        <BottomNavLink key={item.to} item={item} inboxCount={inboxCount} />
      ))}

      {/* Center raised action: ＋交辦 */}
      <div className="flex flex-1 items-center justify-center">
        <button
          type="button"
          // TODO(v2-V5): route to the task board's create intent until a global
          // create-task modal exists.
          onClick={() => navigate('/tasks?new=1')}
          aria-label={intl.formatMessage({ id: 'sidebar.newTask' })}
          title={intl.formatMessage({ id: 'sidebar.newTask' })}
          className="-mt-6 grid h-14 w-14 place-items-center rounded-full bg-gradient-to-b from-amber-400 to-amber-500 text-white shadow-[0_6px_18px_-4px_rgba(245,158,11,0.7)] ring-4 ring-[var(--app-bg)] transition-transform active:scale-95"
        >
          <Plus className="h-6 w-6" />
        </button>
      </div>

      {right.map((item) => (
        <BottomNavLink key={item.to} item={item} inboxCount={inboxCount} />
      ))}
    </nav>
  );
}
