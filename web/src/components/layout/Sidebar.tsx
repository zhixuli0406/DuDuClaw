import { NavLink } from 'react-router';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import {
  LayoutDashboard,
  Bot,
  KanbanSquare,
  GitFork,
  Network,
  Puzzle,
  Radio,
  Wallet,
  Brain,
  BookOpen,
  Shield,
  Settings,
  FileText,
  MessageCircle,
  BarChart3,
  CreditCard,
  KeyRound,
  Building2,
  Users,
  Plug,
  Globe,
  Store,
  Handshake,
  Cpu,
  Scale,
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
  { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks' },
  { to: '/forks', icon: GitFork, label: 'nav.forks' },
  { to: '/org', icon: Network, label: 'nav.org', minRole: 'manager' },
  { to: '/skills', icon: Puzzle, label: 'nav.skills' },
  { to: '/marketplace', icon: Store, label: 'nav.marketplace' },
  { to: '/partner', icon: Handshake, label: 'nav.partner', minRole: 'manager' },
  { to: '/channels', icon: Radio, label: 'nav.channels', minRole: 'admin' },
  { to: '/accounts', icon: Wallet, label: 'nav.accounts', minRole: 'admin' },
  { to: '/memory', icon: Brain, label: 'nav.memory' },
  { to: '/wiki', icon: BookOpen, label: 'nav.wiki' },
  { to: '/shared-wiki', icon: Globe, label: 'nav.sharedWiki' },
  { to: '/wiki-trust', icon: Shield, label: 'nav.wikiTrust', minRole: 'admin' },
  { to: '/security', icon: Shield, label: 'nav.security', minRole: 'admin' },
  { to: '/governance', icon: Scale, label: 'nav.governance', minRole: 'admin' },
  { to: '/reports', icon: BarChart3, label: 'nav.reports', minRole: 'manager' },
  { to: '/billing', icon: CreditCard, label: 'nav.billing', minRole: 'manager' },
  { to: '/license', icon: KeyRound, label: 'nav.license', minRole: 'manager' },
  { to: '/mcp', icon: Plug, label: 'nav.mcp', minRole: 'admin' },
  { to: '/mcp-keys', icon: KeyRound, label: 'nav.mcpKeys', minRole: 'admin' },
  { to: '/odoo', icon: Building2, label: 'nav.odoo', minRole: 'admin' },
  { to: '/inference', icon: Cpu, label: 'nav.inference', minRole: 'admin' },
  { to: '/users', icon: Users, label: 'nav.users', minRole: 'admin' },
  { to: '/settings', icon: Settings, label: 'nav.settings', minRole: 'admin' },
  { to: '/logs', icon: FileText, label: 'nav.logs', minRole: 'manager' },
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
    <aside className="glass-chrome relative z-40 flex w-60 flex-col border-r border-stone-300/40 dark:border-white/8">
      {/* Logo */}
      <div className="flex items-center gap-3 px-5 py-5">
        <span
          className="grid h-10 w-10 place-items-center rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 text-xl shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
          role="img"
          aria-label="paw"
        >
          🐾
        </span>
        <div>
          <h1 className="text-lg font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            DuDuClaw
          </h1>
          <p className="text-xs text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'app.subtitle' })}
          </p>
        </div>
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-0.5 overflow-y-auto px-3 py-2">
        {filteredNavItems.map(({ to, icon: Icon, label }) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            className={({ isActive }) =>
              cn(
                'group relative flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
                isActive
                  ? 'bg-amber-500/12 text-amber-700 shadow-[inset_0_1px_0_0_rgba(255,255,255,0.25)] ring-1 ring-inset ring-amber-500/25 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/20'
                  : 'text-stone-600 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200'
              )
            }
          >
            {({ isActive }) => (
              <>
                {/* Active indicator beam */}
                <span
                  className={cn(
                    'absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-amber-500 transition-opacity dark:bg-amber-400',
                    isActive ? 'opacity-100' : 'opacity-0'
                  )}
                  aria-hidden="true"
                />
                <Icon className="h-[1.125rem] w-[1.125rem] shrink-0" />
                <span>{intl.formatMessage({ id: label })}</span>
              </>
            )}
          </NavLink>
        ))}
      </nav>

      {/* User Info + Footer */}
      <div className="border-t border-stone-300/40 px-4 py-3 dark:border-white/8">
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
