import { useEffect, useMemo, useState } from 'react';
import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import {
  ChevronDown,
  LogOut,
  Search,
  Plus,
  PanelLeftClose,
  PanelLeftOpen,
  Pause,
  Play,
  ArrowRight,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSidebarStore } from '@/stores/sidebar-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useGrowthStore } from '@/stores/growth-store';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { useForksExist } from '@/hooks/useForksExist';
import { useBrandingStore, useEffectiveName, useEffectiveLogo } from '@/lib/branding';
import { CharacterAvatar, agentPose } from '@/components/character';
import {
  dailyItems,
  navGroups,
  staffEntry,
  manageEntry,
  type NavItem,
} from './nav-model';
import { EditionBadge } from './EditionBadge';

const GROUP_COLLAPSE_KEY = 'duduclaw:ui:nav-collapsed';
const RAIL_KEY = 'duduclaw:ui:sidebar-rail';

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

/** How many staff to surface in the live zone before deferring to "全部員工 →". */
const LIVE_LIMIT = 5;
const RECENT_LIMIT = 3;

/** A single nav row (icon + label + desc + optional inbox badge). */
function NavRow({
  item,
  count,
  compact,
}: {
  item: NavItem;
  count: number;
  compact?: boolean;
}) {
  const intl = useIntl();
  const Icon = item.icon;
  const label = intl.formatMessage({ id: item.label });
  return (
    <NavLink
      to={item.to}
      end={item.to === '/'}
      data-tour={`nav:${item.to}`}
      title={compact ? label : undefined}
      className={({ isActive }) =>
        cn(
          'group relative flex items-start rounded-lg text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
          compact ? 'justify-center p-2.5' : 'gap-3 px-3 py-2',
          isActive
            ? 'bg-amber-500/12 text-amber-700 ring-1 ring-inset ring-amber-500/25 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/20'
            : 'text-stone-600 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200',
        )
      }
    >
      {({ isActive }) => (
        <>
          {!compact && (
            <span
              className={cn(
                'absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-amber-500 transition-opacity dark:bg-amber-400',
                isActive ? 'opacity-100' : 'opacity-0',
              )}
              aria-hidden="true"
            />
          )}
          <span className="relative">
            <Icon className={cn('shrink-0', compact ? 'h-5 w-5' : 'mt-0.5 h-[1.125rem] w-[1.125rem]')} />
            {compact && count > 0 && (
              <span className="absolute -right-1.5 -top-1.5 h-2 w-2 rounded-full bg-rose-500" aria-hidden="true" />
            )}
          </span>
          {!compact && (
            <>
              <span className="min-w-0 flex-1">
                <span className="block truncate leading-tight">{label}</span>
                <span
                  className="mt-0.5 block truncate text-[11px] font-normal leading-tight text-stone-400 dark:text-stone-500"
                  title={intl.formatMessage({ id: item.desc })}
                >
                  {intl.formatMessage({ id: item.desc })}
                </span>
              </span>
              {count > 0 && (
                <span
                  className="mt-0.5 inline-flex min-w-[1.25rem] shrink-0 items-center justify-center rounded-full bg-rose-500 px-1.5 py-0.5 text-[10px] font-semibold tabular-nums leading-none text-white"
                  aria-label={intl.formatMessage({ id: 'nav.approvals.pending' }, { count })}
                >
                  {count > 99 ? '99+' : count}
                </span>
              )}
            </>
          )}
        </>
      )}
    </NavLink>
  );
}

/** A collapsible section header + its items. */
function NavSection({
  label,
  items,
  badge,
  collapsed,
  onToggle,
}: {
  label: string;
  items: NavItem[];
  badge: (b: NavItem['badge']) => number;
  collapsed: boolean;
  onToggle: () => void;
}) {
  const intl = useIntl();
  if (items.length === 0) return null;
  return (
    <div className="pt-3">
      <button
        onClick={onToggle}
        aria-expanded={!collapsed}
        className="group flex w-full items-center justify-between rounded-md px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-stone-400 transition-colors hover:text-stone-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-500 dark:hover:text-stone-300"
      >
        <span>{intl.formatMessage({ id: label })}</span>
        <ChevronDown className={cn('h-3.5 w-3.5 transition-transform', collapsed && '-rotate-90')} />
      </button>
      {!collapsed && (
        <div className="mt-0.5 space-y-0.5">
          {items.map((item) => (
            <NavRow key={item.to} item={item} count={badge(item.badge)} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * The LIVE 員工 zone (paperclip P4 + §4.2). Subscribes to the agents store:
 * when any staff are `active` it lists those (with a live dot + working pose);
 * otherwise it shows the most recent few. Each row is a character avatar + name
 * linking to the detail page, with a hover pause/resume affordance. Always ends
 * with the "全部員工 →" link.
 */
function StaffZone({ collapsed, onToggle }: { collapsed: boolean; onToggle: () => void }) {
  const intl = useIntl();
  const agents = useAgentsStore((s) => s.agents);
  const pauseAgent = useAgentsStore((s) => s.pauseAgent);
  const resumeAgent = useAgentsStore((s) => s.resumeAgent);

  const shown = useMemo(() => {
    const active = agents.filter((a) => a.status === 'active');
    if (active.length > 0) return active.slice(0, LIVE_LIMIT);
    return agents.slice(0, RECENT_LIMIT);
  }, [agents]);

  return (
    <div className="pt-3">
      <button
        onClick={onToggle}
        aria-expanded={!collapsed}
        className="group flex w-full items-center justify-between rounded-md px-3 py-1 text-[11px] font-semibold uppercase tracking-wider text-stone-400 transition-colors hover:text-stone-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-500 dark:hover:text-stone-300"
      >
        <span>{intl.formatMessage({ id: 'navGroup.staff' })}</span>
        <ChevronDown className={cn('h-3.5 w-3.5 transition-transform', collapsed && '-rotate-90')} />
      </button>
      {!collapsed && (
        <div className="mt-0.5 space-y-0.5">
          {shown.length === 0 ? (
            <p className="px-3 py-1.5 text-[11px] text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'sidebar.noStaff' })}
            </p>
          ) : (
            shown.map((a) => {
              const isActive = a.status === 'active';
              return (
                <div key={a.name} className="group/staff relative">
                  <NavLink
                    to={`/agents/${encodeURIComponent(a.name)}`}
                    className={({ isActive: navActive }) =>
                      cn(
                        'flex items-center gap-2.5 rounded-lg px-3 py-1.5 text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                        navActive
                          ? 'bg-amber-500/12 text-amber-700 dark:bg-amber-400/10 dark:text-amber-300'
                          : 'text-stone-600 hover:bg-stone-500/8 hover:text-stone-900 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200',
                      )
                    }
                  >
                    <CharacterAvatar
                      agentId={a.name}
                      name={a.display_name}
                      size={26}
                      pose={agentPose(a.status, isActive)}
                      live={isActive}
                    />
                    <span className="min-w-0 flex-1 truncate">{a.display_name}</span>
                  </NavLink>
                  <button
                    type="button"
                    onClick={() => (isActive ? pauseAgent(a.name) : resumeAgent(a.name))}
                    title={intl.formatMessage({ id: isActive ? 'sidebar.pause' : 'sidebar.resume' })}
                    aria-label={intl.formatMessage({ id: isActive ? 'sidebar.pause' : 'sidebar.resume' })}
                    className="absolute right-1.5 top-1/2 hidden -translate-y-1/2 rounded-md p-1 text-stone-400 hover:bg-stone-500/10 hover:text-stone-700 group-hover/staff:block dark:hover:text-stone-200"
                  >
                    {isActive ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
                  </button>
                </div>
              );
            })
          )}
          <NavLink
            to={staffEntry.to}
            data-tour={`nav:${staffEntry.to}`}
            className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[11px] font-medium text-amber-600 transition-colors hover:bg-amber-500/8 hover:text-amber-700 dark:text-amber-400"
          >
            {intl.formatMessage({ id: 'sidebar.allStaff' })}
            <ArrowRight className="h-3 w-3" />
          </NavLink>
        </div>
      )}
    </div>
  );
}

export function Sidebar() {
  const intl = useIntl();
  const navigate = useNavigate();
  const status = useSystemStore((s) => s.status);
  const user = useAuthStore((s) => s.user);
  const bindings = useAuthStore((s) => s.bindings);
  const logout = useAuthStore((s) => s.logout);
  const openPalette = useCommandPaletteStore((s) => s.openPalette);

  const hasOperatorAccess = bindings.some(
    (b) => b.access_level === 'owner' || b.access_level === 'operator',
  );
  const mobileOpen = useSidebarStore((s) => s.mobileOpen);

  const [collapsedSections, setCollapsedSections] = useState<Record<string, boolean>>(() =>
    loadJson(GROUP_COLLAPSE_KEY, {}),
  );
  const [rail, setRail] = useState<boolean>(() => loadJson(RAIL_KEY, false));

  const connectionState = useConnectionStore((s) => s.state);
  const inboxCount = useApprovalsStore((s) => s.pendingCount);
  const fetchInboxCount = useApprovalsStore((s) => s.fetchCount);

  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const onlineCount = agents.filter((a) => a.status === 'active').length;
  const companyLevel = useGrowthStore((s) => s.snapshot?.level ?? null);
  const brandName = useEffectiveName();
  const brandLogo = useEffectiveLogo();
  const brandSubtitle = useBrandingStore((s) => s.branding?.subtitle?.trim() || '');

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchInboxCount();
    fetchAgents();
    const interval = setInterval(fetchInboxCount, 60_000);
    return () => clearInterval(interval);
  }, [connectionState, fetchInboxCount, fetchAgents]);

  const badgeCount = (badge: NavItem['badge']): number => (badge === 'inbox' ? inboxCount : 0);

  const toggleSection = (label: string) => {
    setCollapsedSections((prev) => {
      const next = { ...prev, [label]: !prev[label] };
      try {
        localStorage.setItem(GROUP_COLLAPSE_KEY, JSON.stringify(next));
      } catch {
        /* ignore quota / private-mode failures */
      }
      return next;
    });
  };

  const toggleRail = () => {
    setRail((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(RAIL_KEY, JSON.stringify(next));
      } catch {
        /* ignore */
      }
      return next;
    });
  };

  const isPersonal = status?.edition_profile === 'personal';
  // Only probe fork existence for viewers who could see the entry at all.
  const forksExist = useForksExist(hasMinRole(user?.role, 'manager'));
  const ctx = { hasOperatorAccess, forksExist };

  const workItems = filterVisible(navGroups[0].items, user?.role, isPersonal, ctx);
  const companyItems = filterVisible(navGroups[1].items, user?.role, isPersonal, ctx);
  const canManage = hasMinRole(user?.role, manageEntry.minRole);

  // Icon-rail curated set (collapsed desktop mode). Tasks was demoted from the
  // primary nav (2026-07-12 meeting), so the rail no longer carries it either.
  const railItems: NavItem[] = [
    ...dailyItems,
    staffEntry,
    ...companyItems.filter((i) => ['/memory', '/growth'].includes(i.to)),
    ...(canManage ? [manageEntry] : []),
  ];

  return (
    <aside
      className={cn(
        'glass-chrome z-50 flex shrink-0 flex-col border-r border-stone-300/40 dark:border-white/8',
        rail ? 'w-16' : 'w-60',
        'fixed inset-y-0 left-0 transition-transform duration-200 md:static md:z-40 md:translate-x-0',
        mobileOpen ? 'translate-x-0 shadow-2xl' : '-translate-x-full md:shadow-none',
      )}
    >
      {/* Company nameplate */}
      <div className={cn('flex items-center gap-2 px-3 py-4', rail && 'flex-col px-0')}>
        {brandLogo.isImage ? (
          <img
            src={brandLogo.value}
            alt={brandName}
            className="h-9 w-9 shrink-0 rounded-xl object-cover shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
          />
        ) : (
          <span
            className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 text-lg shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
            role="img"
            aria-label={brandName}
          >
            {brandLogo.value}
          </span>
        )}
        {!rail && (
          <>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5">
                <h1 className="truncate text-base font-semibold tracking-tight text-stone-900 dark:text-stone-50">
                  {brandName}
                </h1>
                {/* Company level — real value from the growth snapshot (V10);
                    falls back to a placeholder until the first snapshot lands. */}
                <span
                  className="rounded-full bg-amber-500/12 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700 dark:bg-amber-400/10 dark:text-amber-300"
                  title={intl.formatMessage({ id: 'sidebar.level.soon' })}
                >
                  {companyLevel == null
                    ? intl.formatMessage({ id: 'sidebar.level.placeholder' })
                    : `Lv.${companyLevel}`}
                </span>
              </div>
              <p className="truncate text-xs text-stone-500 dark:text-stone-400">
                {brandSubtitle || intl.formatMessage({ id: 'app.subtitle' })}
              </p>
            </div>
            <button
              onClick={openPalette}
              title={intl.formatMessage({ id: 'cmdk.title' })}
              aria-label={intl.formatMessage({ id: 'cmdk.title' })}
              className="rounded-lg p-1.5 text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200"
            >
              <Search className="h-4 w-4" />
            </button>
          </>
        )}
        <button
          onClick={toggleRail}
          title={intl.formatMessage({ id: rail ? 'sidebar.expand' : 'sidebar.collapse' })}
          aria-label={intl.formatMessage({ id: rail ? 'sidebar.expand' : 'sidebar.collapse' })}
          className="hidden rounded-lg p-1.5 text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 md:block dark:text-stone-400 dark:hover:text-stone-200"
        >
          {rail ? <PanelLeftOpen className="h-4 w-4" /> : <PanelLeftClose className="h-4 w-4" />}
        </button>
      </div>

      {/* Primary action: 交辦任務 */}
      <div className={cn('px-3 pb-1', rail && 'px-2')}>
        <button
          // TODO(v2-V5): no global create-task modal yet — route to /tasks?new=1.
          onClick={() => navigate('/tasks?new=1')}
          title={intl.formatMessage({ id: 'sidebar.newTask' })}
          className={cn(
            'flex w-full items-center justify-center gap-2 rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 py-2 text-sm font-semibold text-white shadow-[0_4px_14px_-4px_rgba(245,158,11,0.6)] transition-transform hover:brightness-105 active:scale-[0.98] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50',
            rail && 'px-0',
          )}
        >
          <Plus className="h-4 w-4 shrink-0" />
          {!rail && intl.formatMessage({ id: 'sidebar.newTask' })}
        </button>
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-0.5 overflow-y-auto px-3 pb-3">
        {rail ? (
          <div className="mt-2 space-y-1">
            {railItems.map((item) => (
              <NavRow key={item.to} item={item} count={badgeCount(item.badge)} compact />
            ))}
          </div>
        ) : (
          <>
            {/* Flat daily items (no header) */}
            <div className="mt-1 space-y-0.5">
              {dailyItems.map((item) => (
                <NavRow key={item.to} item={item} count={badgeCount(item.badge)} />
              ))}
            </div>

            {/* 工作 */}
            <NavSection
              label="navGroup.work"
              items={workItems}
              badge={badgeCount}
              collapsed={!!collapsedSections['navGroup.work']}
              onToggle={() => toggleSection('navGroup.work')}
            />

            {/* 員工 (live zone) */}
            <StaffZone
              collapsed={!!collapsedSections['navGroup.staff']}
              onToggle={() => toggleSection('navGroup.staff')}
            />

            {/* 公司 (+ 管理) */}
            <NavSection
              label="navGroup.company"
              items={canManage ? [...companyItems, manageEntry] : companyItems}
              badge={badgeCount}
              collapsed={!!collapsedSections['navGroup.company']}
              onToggle={() => toggleSection('navGroup.company')}
            />
          </>
        )}
      </nav>

      {/* Live footer block (paperclip P4): online staff + needs-me count. */}
      {!rail && (
        <div className="border-t border-stone-300/40 px-4 py-2.5 dark:border-white/8">
          <div className="flex items-center justify-between text-xs">
            <span className="flex items-center gap-1.5 text-stone-500 dark:text-stone-400">
              <span className={cn('h-1.5 w-1.5 rounded-full', onlineCount > 0 ? 'bg-emerald-500' : 'bg-stone-400')} />
              {intl.formatMessage({ id: 'sidebar.online' }, { count: onlineCount })}
            </span>
            {inboxCount > 0 && (
              <NavLink to="/inbox" className="flex items-center gap-1 font-medium text-amber-600 hover:text-amber-700 dark:text-amber-400">
                {intl.formatMessage({ id: 'sidebar.needsMe' }, { count: inboxCount })}
              </NavLink>
            )}
          </div>
        </div>
      )}

      {/* User Info + Footer */}
      <div className={cn('border-t border-stone-300/40 px-4 py-3 dark:border-white/8', rail && 'px-2')}>
        {user && !rail && (
          <div className="mb-2 flex items-center justify-between">
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-stone-700 dark:text-stone-300">
                {user.display_name}
              </p>
              <p className="truncate text-xs text-stone-400 dark:text-stone-500">{user.role}</p>
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
        {rail ? (
          <button
            onClick={logout}
            title={intl.formatMessage({ id: 'auth.logout' })}
            aria-label={intl.formatMessage({ id: 'auth.logout' })}
            className="mx-auto flex rounded p-1.5 text-stone-400 hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
          >
            <LogOut className="h-4 w-4" />
          </button>
        ) : (
          <div className="flex items-center justify-between gap-2">
            <p className="font-mono text-[11px] tracking-wide text-stone-400 dark:text-stone-500">
              {status?.version ?? 'v0.12.0'}
            </p>
            <EditionBadge />
          </div>
        )}
      </div>
    </aside>
  );
}
