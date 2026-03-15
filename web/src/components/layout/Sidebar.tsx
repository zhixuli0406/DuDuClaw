import { NavLink } from 'react-router';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  LayoutDashboard,
  Bot,
  Radio,
  Wallet,
  Brain,
  Shield,
  Settings,
  ScrollText,
} from 'lucide-react';

const navItems = [
  { to: '/', icon: LayoutDashboard, label: 'nav.dashboard' },
  { to: '/agents', icon: Bot, label: 'nav.agents' },
  { to: '/channels', icon: Radio, label: 'nav.channels' },
  { to: '/accounts', icon: Wallet, label: 'nav.accounts' },
  { to: '/memory', icon: Brain, label: 'nav.memory' },
  { to: '/security', icon: Shield, label: 'nav.security' },
  { to: '/settings', icon: Settings, label: 'nav.settings' },
  { to: '/logs', icon: ScrollText, label: 'nav.logs' },
] as const;

export function Sidebar() {
  const intl = useIntl();

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
      <nav className="flex-1 space-y-1 px-3 py-2">
        {navItems.map(({ to, icon: Icon, label }) => (
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

      {/* Footer */}
      <div className="border-t border-stone-200 px-5 py-3 dark:border-stone-800">
        <p className="text-xs text-stone-400 dark:text-stone-500">v0.1.0</p>
      </div>
    </aside>
  );
}
