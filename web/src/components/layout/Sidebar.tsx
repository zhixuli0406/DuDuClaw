import { NavLink } from 'react-router';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import {
  LayoutDashboard,
  Bot,
  Network,
  Puzzle,
  Radio,
  Wallet,
  Brain,
  BookOpen,
  Shield,
  Settings,
  ScrollText,
  KeyRound,
  MessageCircle,
  BarChart3,
  CreditCard,
  Building2,
  Users,
  LogOut,
} from 'lucide-react';

type NavItem = {
  to: string;
  icon: typeof LayoutDashboard;
  label: string;
  /** Minimum role required to see this item. Omit for all roles. */
  minRole?: UserRole;
};

const navItems: NavItem[] = [
  { to: '/', icon: LayoutDashboard, label: 'nav.dashboard' },
  { to: '/webchat', icon: MessageCircle, label: 'nav.webchat' },
  { to: '/agents', icon: Bot, label: 'nav.agents' },
  { to: '/org', icon: Network, label: 'nav.org', minRole: 'manager' },
  { to: '/skills', icon: Puzzle, label: 'nav.skills' },
  { to: '/channels', icon: Radio, label: 'nav.channels', minRole: 'admin' },
  { to: '/accounts', icon: Wallet, label: 'nav.accounts', minRole: 'admin' },
  { to: '/memory', icon: Brain, label: 'nav.memory' },
  { to: '/wiki', icon: BookOpen, label: 'nav.wiki' },
  { to: '/security', icon: Shield, label: 'nav.security', minRole: 'admin' },
  { to: '/reports', icon: BarChart3, label: 'nav.reports', minRole: 'manager' },
  { to: '/billing', icon: CreditCard, label: 'nav.billing', minRole: 'manager' },
  { to: '/odoo', icon: Building2, label: 'nav.odoo', minRole: 'admin' },
  { to: '/users', icon: Users, label: 'nav.users', minRole: 'admin' },
  { to: '/settings', icon: Settings, label: 'nav.settings', minRole: 'admin' },
  { to: '/license', icon: KeyRound, label: 'nav.license', minRole: 'admin' },
  { to: '/logs', icon: ScrollText, label: 'nav.logs', minRole: 'manager' },
];

export function Sidebar() {
  const intl = useIntl();
  const status = useSystemStore((s) => s.status);
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);

  const filteredNavItems = navItems.filter((item) =>
    hasMinRole(user?.role, item.minRole)
  );

  return (
    <aside className="flex w-60 flex-col border-r border-stone-200 bg-stone-50/80 backdrop-blur-xl dark:border-stone-800 dark:bg-stone-900/80">
      {/* Logo */}
      <div className="flex items-center gap-3 px-5 py-5">
        <span className="text-2xl" role="img" aria-label="paw">
          🐾
        </span>
        <div>
          <h1 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="text-xs text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'app.subtitle' })}
          </p>
        </div>
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-1 overflow-y-auto px-3 py-2">
        {filteredNavItems.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            className={({ isActive }) =>
              cn(
                'flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-colors',
                isActive
                  ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                  : 'text-stone-600 hover:bg-stone-100 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-stone-800 dark:hover:text-stone-200'
              )
            }
          >
            <Icon className="h-[1.125rem] w-[1.125rem] shrink-0" />
            <span>{intl.formatMessage({ id: label })}</span>
          </NavLink>
        ))}
      </nav>

      {/* User Info + Footer */}
      <div className="border-t border-stone-200 px-4 py-3 dark:border-stone-800">
        {user && (
          <div className="mb-2 flex items-center justify-between">
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-stone-700 dark:text-stone-300">
                {user.display_name}
              </p>
              <p className="truncate text-xs text-stone-400 dark:text-stone-500">
                {user.role}
              </p>
            </div>
            <button
              onClick={logout}
              className="rounded p-1.5 text-stone-400 transition-colors hover:bg-stone-100 hover:text-stone-600 dark:hover:bg-stone-800 dark:hover:text-stone-300"
              title={intl.formatMessage({ id: 'auth.logout' })}
            >
              <LogOut className="h-4 w-4" />
            </button>
          </div>
        )}
        <p className="text-xs text-stone-400 dark:text-stone-500">{status?.version ?? 'v0.12.0'}</p>
      </div>
    </aside>
  );
}
