import { useEffect, useMemo, useState } from 'react';
import { NavLink, useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import {
  Plus,
  Search,
  ChevronDown,
  Sun,
  Moon,
  Monitor,
  LogOut,
  Bell,
  Languages,
  ArrowRight,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useGrowthStore } from '@/stores/growth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useLocaleStore, localeNames } from '@/i18n';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { useForksExist } from '@/hooks/useForksExist';
import { useEffectiveName, useEffectiveLogo } from '@/lib/branding';
import { useTodayCost } from '@/components/growth/useTodayCost';
import { CoinChip } from '@/components/ui';
import {
  SidebarHeader,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuBadge,
  SidebarRail,
  sidebarMenuButtonVariants,
  useSidebar,
  ActorAvatar,
  Button,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  type ActorStatus,
} from '@/components/mds';
import {
  dailyItems,
  navGroups,
  staffEntry,
  type NavItem,
} from './nav-model';
import { EditionBadge } from './EditionBadge';

const GROUP_COLLAPSE_KEY = 'duduclaw:ui:nav-collapsed';
const LIVE_LIMIT = 5;
const RECENT_LIMIT = 3;

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

/** Spec §5.1 active rule: exact for the index route, prefix match otherwise. */
function useNavClass(collapsed: boolean) {
  return ({ isActive }: { isActive: boolean }) =>
    cn(
      sidebarMenuButtonVariants(),
      collapsed ? 'justify-center' : 'px-2',
      isActive && 'bg-sidebar-accent font-medium text-sidebar-accent-foreground',
    );
}

/** Best-effort platform hint for the ⌘K vs Ctrl+K keycap. */
function isMacLike(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /Mac|iPhone|iPad|iPod/i.test(`${navigator.platform ?? ''} ${navigator.userAgent ?? ''}`);
}

/** A single nav row (mds SidebarMenuButton styling on a real NavLink). */
function NavRow({ item, count, collapsed }: { item: NavItem; count: number; collapsed: boolean }) {
  const intl = useIntl();
  const Icon = item.icon;
  const label = intl.formatMessage({ id: item.label });
  const navClass = useNavClass(collapsed);
  return (
    <SidebarMenuItem>
      <NavLink
        to={item.to}
        end={item.to === '/'}
        data-tour={`nav:${item.to}`}
        title={collapsed ? label : undefined}
        className={navClass}
      >
        <span className="relative flex shrink-0">
          <Icon />
          {collapsed && count > 0 && (
            <span className="absolute -right-1 -top-1 size-1.5 rounded-full bg-brand" aria-hidden="true" />
          )}
        </span>
        {!collapsed && (
          <>
            <span className="min-w-0 flex-1 truncate">{label}</span>
            {count > 0 && (
              <SidebarMenuBadge
                className="rounded-full bg-brand px-1.5 font-medium text-brand-foreground"
                aria-label={intl.formatMessage({ id: 'nav.inbox.pending' }, { count })}
              >
                {count > 99 ? '99+' : count}
              </SidebarMenuBadge>
            )}
          </>
        )}
      </NavLink>
    </SidebarMenuItem>
  );
}

/** Collapsible GroupLabel + its menu. Hides entirely when it has no items. */
function NavGroupSection({
  label,
  items,
  badgeFor,
  collapsed,
  sectionCollapsed,
  onToggle,
}: {
  label: string;
  items: NavItem[];
  badgeFor: (b: NavItem['badge']) => number;
  collapsed: boolean;
  sectionCollapsed: boolean;
  onToggle: () => void;
}) {
  const intl = useIntl();
  if (items.length === 0) return null;
  if (collapsed) {
    return (
      <SidebarGroup>
        <SidebarMenu>
          {items.map((item) => (
            <NavRow key={item.to} item={item} count={badgeFor(item.badge)} collapsed />
          ))}
        </SidebarMenu>
      </SidebarGroup>
    );
  }
  return (
    <SidebarGroup>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={!sectionCollapsed}
        className="flex h-8 w-full items-center px-2 text-xs font-medium text-sidebar-foreground/70 outline-none transition-colors hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
      >
        <span className="flex-1 text-left">{intl.formatMessage({ id: label })}</span>
        <ChevronDown className={cn('size-3.5 transition-transform', sectionCollapsed && '-rotate-90')} />
      </button>
      {!sectionCollapsed && (
        <SidebarMenu>
          {items.map((item) => (
            <NavRow key={item.to} item={item} count={badgeFor(item.badge)} collapsed={false} />
          ))}
        </SidebarMenu>
      )}
    </SidebarGroup>
  );
}

const AGENT_STATUS_DOT: Record<string, ActorStatus> = {
  active: 'online',
  paused: 'busy',
};

/** LIVE 員工 zone (spec §5.1): plain ActorAvatar rows, no gradient/立繪. */
function StaffZone({
  collapsed,
  sectionCollapsed,
  onToggle,
}: {
  collapsed: boolean;
  sectionCollapsed: boolean;
  onToggle: () => void;
}) {
  const intl = useIntl();
  const agents = useAgentsStore((s) => s.agents);
  const navClass = useNavClass(collapsed);

  const shown = useMemo(() => {
    const active = agents.filter((a) => a.status === 'active');
    if (active.length > 0) return active.slice(0, LIVE_LIMIT);
    return agents.slice(0, RECENT_LIMIT);
  }, [agents]);

  if (collapsed) {
    if (shown.length === 0) return null;
    return (
      <SidebarGroup>
        <SidebarMenu>
          {shown.map((a) => (
            <SidebarMenuItem key={a.name}>
              <NavLink
                to={`/agents/${encodeURIComponent(a.name)}`}
                title={a.display_name}
                className={navClass}
              >
                <ActorAvatar
                  actorType="agent"
                  size="sm"
                  name={a.display_name}
                  showStatusDot
                  status={AGENT_STATUS_DOT[a.status] ?? 'offline'}
                />
              </NavLink>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
      </SidebarGroup>
    );
  }

  return (
    <SidebarGroup>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={!sectionCollapsed}
        className="flex h-8 w-full items-center px-2 text-xs font-medium text-sidebar-foreground/70 outline-none transition-colors hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
      >
        <span className="flex-1 text-left">{intl.formatMessage({ id: 'navGroup.staff' })}</span>
        <ChevronDown className={cn('size-3.5 transition-transform', sectionCollapsed && '-rotate-90')} />
      </button>
      {!sectionCollapsed && (
        <SidebarMenu>
          {shown.length === 0 ? (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'sidebar.noStaff' })}
            </p>
          ) : (
            shown.map((a) => (
              <SidebarMenuItem key={a.name}>
                <NavLink
                  to={`/agents/${encodeURIComponent(a.name)}`}
                  className={navClass}
                >
                  <ActorAvatar
                    actorType="agent"
                    size="sm"
                    name={a.display_name}
                    showStatusDot
                    status={AGENT_STATUS_DOT[a.status] ?? 'offline'}
                  />
                  <span className="min-w-0 flex-1 truncate">{a.display_name}</span>
                </NavLink>
              </SidebarMenuItem>
            ))
          )}
          <SidebarMenuItem>
            <NavLink
              to={staffEntry.to}
              data-tour={`nav:${staffEntry.to}`}
              className={cn(sidebarMenuButtonVariants({ size: 'sm' }), 'px-2 text-brand hover:text-brand')}
            >
              <span className="flex-1 truncate">{intl.formatMessage({ id: 'sidebar.allStaff' })}</span>
              <ArrowRight className="size-3.5" />
            </NavLink>
          </SidebarMenuItem>
        </SidebarMenu>
      )}
    </SidebarGroup>
  );
}

/** Company / workspace switcher (spec §5.1 header). */
function CompanySwitcher({ collapsed }: { collapsed: boolean }) {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  const brandName = useEffectiveName();
  const brandLogo = useEffectiveLogo();
  const locale = useLocaleStore((s) => s.locale);
  const setLocale = useLocaleStore((s) => s.setLocale);

  const logoNode = brandLogo.isImage ? (
    <img src={brandLogo.value} alt={brandName} className="size-6 shrink-0 rounded-md object-cover" />
  ) : (
    <span className="grid size-6 shrink-0 place-items-center rounded-md bg-sidebar-accent text-sm" role="img" aria-label={brandName}>
      {brandLogo.value}
    </span>
  );

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className={cn(
          'flex w-full items-center gap-2 rounded-md p-1.5 text-left outline-none transition-colors hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring',
          collapsed && 'justify-center',
        )}
        aria-label={intl.formatMessage({ id: 'sidebar.workspace' })}
      >
        {logoNode}
        {!collapsed && (
          <>
            <span className="min-w-0 flex-1 truncate text-sm font-medium text-sidebar-foreground">{brandName}</span>
            <ChevronDown className="size-4 shrink-0 text-muted-foreground" />
          </>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-56">
        {user && (
          <DropdownMenuLabel className="flex flex-col gap-0.5">
            <span className="truncate text-sm font-medium text-foreground">{user.display_name}</span>
            <span className="truncate text-xs font-normal text-muted-foreground">{user.role}</span>
          </DropdownMenuLabel>
        )}
        <DropdownMenuSeparator />
        <DropdownMenuLabel>{intl.formatMessage({ id: 'header.language' })}</DropdownMenuLabel>
        {Object.entries(localeNames).map(([code, name]) => (
          <DropdownMenuItem
            key={code}
            onClick={() => setLocale(code)}
            className={cn(locale === code && 'font-medium text-foreground')}
          >
            <Languages className="text-muted-foreground" />
            <span className="flex-1">{name}</span>
            {locale === code && <span className="text-brand">•</span>}
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onClick={() => logout()}>
          <LogOut />
          {intl.formatMessage({ id: 'auth.logout' })}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Search trigger with ⌘K keycaps (spec §5.1). */
function SearchTrigger({ collapsed }: { collapsed: boolean }) {
  const intl = useIntl();
  const openPalette = useCommandPaletteStore((s) => s.openPalette);
  const modKey = isMacLike() ? '⌘' : 'Ctrl';
  return (
    <button
      type="button"
      onClick={openPalette}
      aria-label={intl.formatMessage({ id: 'cmdk.title' })}
      title={intl.formatMessage({ id: 'cmdk.title' })}
      className={cn(
        'flex h-8 w-full items-center gap-2 rounded-lg border border-input bg-transparent px-2.5 text-sm text-muted-foreground outline-none transition-colors hover:bg-muted focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30',
        collapsed && 'justify-center px-0',
      )}
    >
      <Search className="size-4 shrink-0" />
      {!collapsed && (
        <>
          <span className="flex-1 text-left">{intl.formatMessage({ id: 'cmdk.trigger' })}</span>
          <kbd className="inline-flex items-center gap-0.5 rounded border border-border px-1 py-0.5 font-mono text-[10px] leading-none">
            {modKey}K
          </kbd>
        </>
      )}
    </button>
  );
}

/** Footer edition card carrying today's spend + company level (spec §5.1). */
function EditionCard() {
  const intl = useIntl();
  const navigate = useNavigate();
  const role = useAuthStore((s) => s.user?.role);
  const canSeeCost = hasMinRole(role, 'manager');
  const { cents, mode } = useTodayCost({ enabled: canSeeCost });
  const companyLevel = useGrowthStore((s) => s.snapshot?.level ?? null);

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-sidebar-border p-2">
      <div className="flex items-center justify-between gap-2">
        <EditionBadge />
        <button
          type="button"
          onClick={() => navigate('/growth')}
          title={intl.formatMessage({ id: 'hud.growth' })}
          className="rounded bg-sidebar-accent px-1.5 py-0.5 font-mono text-[10px] font-medium tabular-nums text-sidebar-foreground/80 transition-colors hover:text-sidebar-foreground"
        >
          {companyLevel == null
            ? intl.formatMessage({ id: 'sidebar.level.placeholder' })
            : `Lv.${companyLevel}`}
        </button>
      </div>
      {canSeeCost && cents !== null && mode !== 'loading' && (
        <CoinChip
          cents={cents}
          onClick={() => navigate('/manage/billing')}
          title={`${intl.formatMessage({ id: mode === 'today' ? 'hud.cost.today' : 'hud.cost.cumulative' })} · ${intl.formatMessage({ id: 'nav.billing' })}`}
        />
      )}
    </div>
  );
}

/**
 * AppSidebar — the composed Multica navigation rail (WP0.4, spec §5.1). Rendered
 * as the children of the mds `<Sidebar variant="inset">` island. Migrates the
 * former global Header duties inline: ⌘K → the header SearchTrigger; theme + bell
 * + connection → the footer; cost/XP → the footer edition card; language + user +
 * logout → the company switcher.
 */
export function AppSidebar() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { state, isMobile } = useSidebar();
  const collapsed = state === 'collapsed' && !isMobile;

  const user = useAuthStore((s) => s.user);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const bindings = useAuthStore((s) => s.bindings);
  const hasOperatorAccess = bindings.some(
    (b) => b.access_level === 'owner' || b.access_level === 'operator',
  );

  const connectionState = useConnectionStore((s) => s.state);
  const inboxCount = useApprovalsStore((s) => s.pendingCount);
  const fetchInboxCount = useApprovalsStore((s) => s.fetchCount);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const theme = useThemeStore((s) => s.theme);
  const cycleTheme = useThemeStore((s) => s.cycleTheme);

  const [collapsedSections, setCollapsedSections] = useState<Record<string, boolean>>(() =>
    loadJson(GROUP_COLLAPSE_KEY, {}),
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchInboxCount();
    fetchAgents();
    const interval = setInterval(fetchInboxCount, 60_000);
    return () => clearInterval(interval);
  }, [connectionState, fetchInboxCount, fetchAgents]);

  const forksExist = useForksExist(hasMinRole(user?.role, 'manager'));
  const ctx = { hasOperatorAccess, forksExist };

  const workItems = filterVisible(navGroups[0].items, user?.role, isPersonal, ctx);
  const companyItems = filterVisible(navGroups[1].items, user?.role, isPersonal, ctx);
  const settingsItems = filterVisible(navGroups[2].items, user?.role, isPersonal, ctx);

  const badgeFor = (badge: NavItem['badge']): number => (badge === 'inbox' ? inboxCount : 0);

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

  const ThemeIcon = theme === 'light' ? Sun : theme === 'dark' ? Moon : Monitor;

  return (
    <>
      <SidebarHeader>
        <CompanySwitcher collapsed={collapsed} />
        {collapsed ? (
          <Button
            variant="brand"
            size="icon"
            className="mx-auto"
            onClick={() => navigate('/tasks?new=1')}
            title={intl.formatMessage({ id: 'sidebar.newTask' })}
            aria-label={intl.formatMessage({ id: 'sidebar.newTask' })}
          >
            <Plus />
          </Button>
        ) : (
          <Button variant="brand" className="w-full" onClick={() => navigate('/tasks?new=1')}>
            <Plus />
            {intl.formatMessage({ id: 'sidebar.newTask' })}
          </Button>
        )}
        <SearchTrigger collapsed={collapsed} />
      </SidebarHeader>

      <SidebarContent>
        {/* Personal group — flat, no group label. */}
        <SidebarGroup>
          <SidebarMenu>
            {dailyItems.map((item) => (
              <NavRow key={item.to} item={item} count={badgeFor(item.badge)} collapsed={collapsed} />
            ))}
          </SidebarMenu>
        </SidebarGroup>

        <NavGroupSection
          label={navGroups[0].label}
          items={workItems}
          badgeFor={badgeFor}
          collapsed={collapsed}
          sectionCollapsed={!!collapsedSections[navGroups[0].label]}
          onToggle={() => toggleSection(navGroups[0].label)}
        />

        <StaffZone
          collapsed={collapsed}
          sectionCollapsed={!!collapsedSections['navGroup.staff']}
          onToggle={() => toggleSection('navGroup.staff')}
        />

        <NavGroupSection
          label={navGroups[1].label}
          items={companyItems}
          badgeFor={badgeFor}
          collapsed={collapsed}
          sectionCollapsed={!!collapsedSections[navGroups[1].label]}
          onToggle={() => toggleSection(navGroups[1].label)}
        />

        <NavGroupSection
          label={navGroups[2].label}
          items={settingsItems}
          badgeFor={badgeFor}
          collapsed={collapsed}
          sectionCollapsed={!!collapsedSections[navGroups[2].label]}
          onToggle={() => toggleSection(navGroups[2].label)}
        />
      </SidebarContent>

      <SidebarFooter>
        {!collapsed && <EditionCard />}
        <div className={cn('flex items-center gap-1', collapsed && 'flex-col')}>
          {/* Notification bell — needs-me inbox quick-jump. */}
          <NavLink
            to="/inbox"
            aria-label={intl.formatMessage({ id: 'nav.inbox' })}
            title={intl.formatMessage({ id: 'nav.inbox' })}
            className={({ isActive }) =>
              cn(
                'relative grid size-8 place-items-center rounded-md outline-none transition-colors hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring',
                isActive ? 'text-sidebar-accent-foreground' : 'text-muted-foreground',
              )
            }
          >
            <Bell className="size-4" />
            {inboxCount > 0 && (
              <span className="absolute right-1 top-1 size-1.5 rounded-full bg-brand" aria-hidden="true" />
            )}
          </NavLink>
          {/* Theme toggle (light → dark → system). */}
          <button
            type="button"
            onClick={cycleTheme}
            title={intl.formatMessage({ id: 'header.theme' }, { theme })}
            aria-label={intl.formatMessage({ id: 'header.theme' }, { theme })}
            className="grid size-8 place-items-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-sidebar-accent/70 hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
          >
            <ThemeIcon className="size-4" />
          </button>
          {/* Connection status dot (click reconnects when disconnected). */}
          <span
            className={cn('ml-auto flex items-center gap-1.5 px-1', collapsed && 'ml-0')}
            title={intl.formatMessage({ id: `status.${connectionState}` })}
          >
            <span
              className={cn(
                'size-1.5 rounded-full',
                connectionState === 'authenticated' || connectionState === 'connected'
                  ? 'bg-success'
                  : connectionState === 'connecting'
                    ? 'animate-pulse bg-warning'
                    : 'bg-destructive',
              )}
            />
          </span>
        </div>
      </SidebarFooter>

      <SidebarRail />
    </>
  );
}
