import { canSelectAgent, useSelectableCallableAgents } from '@/stores/callable-agents-store';
import { TASK_ENTRY_POINTS_HIDDEN, chatEntryLabel } from '@/lib/task-entry-visibility';
import { useVisibleAgents } from '@/lib/data-scope';
import { useEffect, useMemo, useState } from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router';
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
  MoreHorizontal,
} from 'lucide-react';
import { EmployeeAccountMenu } from './EmployeeAccountMenu';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { sortPrimaryStaff } from '@/lib/primary-agent';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { selectInboxCount, useApprovalsStore } from '@/stores/approvals-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useOpenAssign } from '@/hooks/useOpenAssign';
import { ASSIGN_PATH } from '@/lib/assign-entry';
import { useAgentsStore } from '@/stores/agents-store';
import { useGrowthStore } from '@/stores/growth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useLocaleStore, localeNames } from '@/i18n';
import { hasMinRole } from '@/lib/roles';
import { filterVisible } from '@/lib/nav-visibility';
import { useForksExist } from '@/hooks/useForksExist';
import { useIsAppliance } from '@/hooks/useIsAppliance';
import { CompanyLogo, useCompanyName } from '@/components/brand/CompanyLogo';
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
  employeeNavItems,
  isEmployeeSidebar,
  inboxEntry,
  navGroups,
  personalAdvancedGroup,
  personalPrimaryItems,
  staffEntry,
  type NavItem,
  isNewFeature,
} from './nav-model';
import { isTauri } from '@/lib/gateway-picker';
import { EditionBadge } from './EditionBadge';
import { ConversationsZone } from './ConversationsZone';
import { useConversationsStore } from '@/stores/conversations-store';

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
  // 新功能 chip (2026-08-14 convention): shown while the running version is
  // at or below the release the page shipped in; self-expires next minor.
  const version = useSystemStore((s) => s.status?.version ?? null);
  const showNew = isNewFeature(item.newIn, version);
  const navClass = useNavClass(collapsed);
  const startNew = useConversationsStore((s) => s.startNew);
  // Action rows (新對話) run their side effect before the NavLink navigates, so
  // the row does what it says even when the target page is already open.
  const isAction = item.action === 'newConversation';
  const onClick = isAction ? () => startNew() : undefined;
  return (
    <SidebarMenuItem>
      <NavLink
        to={item.to}
        end={item.to === '/'}
        data-tour={`nav:${item.to}`}
        title={collapsed ? label : undefined}
        onClick={onClick}
        className={({ isActive }) =>
          cn(
            navClass({ isActive }),
            // 新對話 is the one row that DOES something rather than going
            // somewhere, and as a plain nav row it disappeared among its
            // neighbours (2026-08-03 feedback). 2026-08-04 (D17) it also moved
            // to the top of the rail and got a brand-tinted chip: still not a
            // filled CTA, but unmistakably the primary action.
            isAction &&
              'border border-brand/30 bg-brand/8 font-medium text-sidebar-foreground hover:bg-brand/14 [&>span>svg]:text-brand',
          )
        }
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
            {showNew && (
              <span className="shrink-0 rounded-full bg-brand/15 px-1.5 py-px text-[10px] font-medium text-brand">
                {intl.formatMessage({ id: 'nav.badge.new' })}
              </span>
            )}
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
  employee = false,
}: {
  collapsed: boolean;
  sectionCollapsed: boolean;
  onToggle: () => void;
  employee?: boolean;
}) {
  const intl = useIntl();
  const agents = useVisibleAgents();
  const selection = useSelectableCallableAgents();
  const bindings = useAuthStore((s) => s.bindings);
  const primaryAgent = useAuthStore((s) => s.user?.primary_agent_id);
  const [settingPrimary, setSettingPrimary] = useState(false);
  const setPrimary = async (agent: string) => {
    if (!canSelectAgent(agent)) return;
    setSettingPrimary(true);
    const subject = useAuthStore.getState().user?.id;
    try {
      await api.users.setPrimaryAgent(agent);
      const me = await api.users.me();
      if (subject && useAuthStore.getState().user?.id === subject) {
        useAuthStore.getState().setUser(me.user, me.bindings);
      }
    } catch (error) { toast.error(formatError(error)); }
    finally { setSettingPrimary(false); }
  };
  const navClass = useNavClass(collapsed);

  const shown = useMemo(() => {
    if (employee) {
      return sortPrimaryStaff(agents, bindings, primaryAgent).slice(0, 8);
    }
    const active = agents.filter((a) => a.status === 'active');
    if (active.length > 0) return active.slice(0, LIVE_LIMIT);
    return agents.slice(0, RECENT_LIMIT);
  }, [agents, bindings, employee, primaryAgent]);

  if (collapsed) {
    if (shown.length === 0 && !employee) return null;
    return (
      <SidebarGroup>
        <SidebarMenu>
          {shown.map((a) => (
            <SidebarMenuItem key={a.name}>
              <NavLink
                to={`/agents/${encodeURIComponent(a.name)}`}
                title={a.readOnly ? intl.formatMessage({ id: 'agents.teamMember' }, { name: a.teamMember }) : a.display_name}
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
          {employee && <SidebarMenuItem>
            <NavLink to={staffEntry.to} className={navClass}
              title={intl.formatMessage({ id: 'sidebar.employeeAllStaff' })}
              aria-label={intl.formatMessage({ id: 'sidebar.employeeAllStaff' })}>
              <ArrowRight className="size-4" />
            </NavLink>
          </SidebarMenuItem>}
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
                  className={({ isActive }) => cn(navClass({ isActive }), employee && "pr-9")}
                >
                  <ActorAvatar
                    actorType="agent"
                    size="sm"
                    name={a.display_name}
                    showStatusDot
                    status={AGENT_STATUS_DOT[a.status] ?? 'offline'}
                  />
                  <span className="min-w-0 flex-1 truncate">{a.display_name}{a.readOnly && <span className="block text-[10px] text-muted-foreground">{intl.formatMessage({ id: 'agents.teamMember' }, { name: a.teamMember })}</span>}</span>
                  {employee && a.name === primaryAgent && <span className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'sidebar.primaryAgent' })}</span>}
                </NavLink>
                {employee && bindings.some(binding => binding.agent_name === a.name && (binding.access_level === 'owner' || binding.access_level === 'operator')) && <DropdownMenu>
                  <DropdownMenuTrigger render={<Button variant="ghost" size="icon" className="absolute right-1 top-1 size-6" aria-label={intl.formatMessage({ id: 'sidebar.setPrimaryAgent' })} />}>
                    <MoreHorizontal className="size-4" />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent>
                    <DropdownMenuItem disabled={settingPrimary || a.name === primaryAgent || !(selection.unrestricted || selection.agents?.some(g => g.name === a.name && g.selectable))} onClick={() => void setPrimary(a.name)}>
                      {intl.formatMessage({ id: 'sidebar.setPrimaryAgent' })}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>}
              </SidebarMenuItem>
            ))
          )}
          <SidebarMenuItem>
            <NavLink
              to={staffEntry.to}
              data-tour={`nav:${staffEntry.to}`}
              className={cn(sidebarMenuButtonVariants({ size: 'sm' }), 'px-2 text-brand hover:text-brand')}
            >
              <span className="flex-1 truncate">{intl.formatMessage({ id: employee ? 'sidebar.employeeAllStaff' : 'sidebar.allStaff' })}</span>
              <ArrowRight className="size-3.5" />
            </NavLink>
          </SidebarMenuItem>
        </SidebarMenu>
      )}
    </SidebarGroup>
  );
}

/** Company / workspace switcher (spec §5.1 header). */
function CompanySwitcher({ collapsed, employee = false }: { collapsed: boolean; employee?: boolean }) {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  // D4-A: on the Personal edition the next page load signs the operator back in
  // automatically (loopback auto-login), so a logout item here would visibly
  // undo itself. Hidden instead of shown-and-broken — password protection is
  // the `local_auto_login` switch, explained in account settings.
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const brandName = useCompanyName();
  const locale = useLocaleStore((s) => s.locale);
  const setLocale = useLocaleStore((s) => s.setLocale);

  // C1 mark at nav size (28 px); no chip behind it (logo usage rules).
  const logoNode = <CompanyLogo size={28} />;

  if (employee) return (
    <div className={cn("flex items-center gap-2 p-1.5", collapsed && "justify-center")} >
      {logoNode}
      {!collapsed && <span className="truncate text-sm font-medium">{brandName}</span>}
    </div>
  );

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className={cn(
          'flex w-full items-center gap-2 rounded-md p-1.5 text-left outline-none transition-colors hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring',
          // Collapsed, the trigger is a 32 px square: tighter padding fits the 28 px mark.
          collapsed && 'justify-center p-0.5',
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
        {!isPersonal && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={() => logout()}>
              <LogOut />
              {intl.formatMessage({ id: 'auth.logout' })}
            </DropdownMenuItem>
          </>
        )}
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

/**
 * Footer edition card: edition badge + company level, closed by the running
 * version number.
 *
 * 2026-08-04 (WP14 client annotation) the running-total spend chip was removed
 * from here. A dollar figure parked in the corner of every screen reads as a
 * meter running against you, and the number people actually needed from that
 * spot — "which build am I on?" — was nowhere in the UI. Spend itself is
 * unchanged and still lives on 管理 → 進階設定 → 帳務.
 */
function EditionCard() {
  const intl = useIntl();
  const navigate = useNavigate();
  const companyLevel = useGrowthStore((s) => s.snapshot?.level ?? null);
  // The gateway's own version, from `system.status` — the same build that is
  // serving this dashboard, not whatever the bundle was compiled against.
  const version = useSystemStore((s) => s.status?.version ?? null);

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
      {version && (
        <p
          className="px-0.5 font-mono text-[10px] leading-none tabular-nums text-sidebar-foreground/60"
          title={intl.formatMessage({ id: 'sidebar.version' }, { version })}
        >
          v{version}
        </p>
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
  const openAssign = useOpenAssign();
  // On the 交辦任務 page the primary button reads as the current page.
  const pathname = useLocation().pathname;
  const onAssign = !TASK_ENTRY_POINTS_HIDDEN && pathname === ASSIGN_PATH;
  const { state, isMobile } = useSidebar();
  const collapsed = state === 'collapsed' && !isMobile;

  const user = useAuthStore((s) => s.user);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const employee = isEmployeeSidebar(user?.role, isPersonal);
  const employeeTasks = useSystemStore((s) => s.status?.employee_tasks) === true;
  const bindings = useAuthStore((s) => s.bindings);
  const hasOperatorAccess = bindings.some(
    (b) => b.access_level === 'owner' || b.access_level === 'operator',
  );

  const connectionState = useConnectionStore((s) => s.state);
  const inboxCount = useApprovalsStore(selectInboxCount);
  const inboxStale = useApprovalsStore((s) => s.stale);
  const inboxStaleLabel = intl.formatMessage({ id: 'liveEvents.stale', defaultMessage: 'Data may be stale' });
  const inboxName = intl.formatMessage({ id: 'nav.inbox' });
  const inboxLabel = inboxStale
    ? `${inboxName} (${inboxStaleLabel})`
    : inboxCount > 0
      ? `${inboxName} (${intl.formatMessage({ id: 'nav.inbox.pending' }, { count: inboxCount })})`
      : inboxName;
  const fetchInboxCount = useApprovalsStore((s) => s.fetchCount);
  const agents = useVisibleAgents();
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
    const interval = setInterval(fetchInboxCount, 5 * 60_000);
    return () => clearInterval(interval);
  }, [connectionState, fetchInboxCount, fetchAgents, user?.id, user?.role]);

  const forksExist = useForksExist(hasMinRole(user?.role, 'manager'));
  // WP-C: `/device` is admin-only server-side too, so only probe for admins —
  // a manager/employee could never reach the RPC anyway.
  const isAppliance = useIsAppliance(hasMinRole(user?.role, 'admin'));
  // D6: the org chart's progressive-disclosure gate reuses the roster that's
  // already being fetched for the LIVE staff zone — no second RPC.
  const ctx = {
    hasOperatorAccess,
    forksExist,
    isDesktop: isTauri(),
    agentCount: agents.length,
    isAppliance,
  };

  const workItems = filterVisible(navGroups[0].items, user?.role, isPersonal, ctx);
  const companyItems = filterVisible(navGroups[1].items, user?.role, isPersonal, ctx);
  const settingsItems = filterVisible(navGroups[2].items, user?.role, isPersonal, ctx);
  const orgItems = filterVisible(navGroups[3].items, user?.role, isPersonal, ctx);
  // The daily row is shared by both editions; Personal adds a second flat row
  // BELOW the 對話紀錄 zone so the zone lands where 對話 used to sit.
  const dailyRow = filterVisible(employee ? employeeNavItems(employeeTasks) : dailyItems, user?.role, isPersonal, ctx)
    .filter((item) => !TASK_ENTRY_POINTS_HIDDEN || item.action !== 'newConversation');
  const personalRow = filterVisible(personalPrimaryItems, user?.role, isPersonal, ctx);
  const advancedItems = filterVisible(personalAdvancedGroup.items, user?.role, isPersonal, ctx);

  const badgeFor = (badge: NavItem['badge']): number => (badge === 'inbox' ? inboxCount : 0);

  // Personal defaults 設定 and 進階 to collapsed; an explicit user toggle
  // (stored, even as `false`) always wins over the default.
  const personalDefaultCollapsed = new Set(['navGroup.settings', 'navGroup.advanced']);
  const isSectionCollapsed = (label: string): boolean =>
    collapsedSections[label] ?? (isPersonal && personalDefaultCollapsed.has(label));

  const toggleSection = (label: string) => {
    setCollapsedSections((prev) => {
      const next = { ...prev, [label]: !isSectionCollapsed(label) };
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
        <CompanySwitcher collapsed={collapsed} employee={employee} />
        {employee && <SearchTrigger collapsed={collapsed} />}
        {/* The primary button owns the single new-conversation entry. */}
        {collapsed ? (
          <Button
            variant="brand"
            size="icon"
            className={cn('mx-auto', onAssign && 'ring-2 ring-brand/40 ring-offset-1 ring-offset-sidebar')}
            data-tour="nav:/chat"
            onClick={() => openAssign()}
            aria-current={onAssign ? 'page' : undefined}
            title={intl.formatMessage({ id: chatEntryLabel('sidebar.newTask') })}
            aria-label={intl.formatMessage({ id: chatEntryLabel('sidebar.newTask') })}
          >
            <Plus />
          </Button>
        ) : (
          <Button
            variant="brand"
            className={cn('w-full', onAssign && 'ring-2 ring-brand/40 ring-offset-1 ring-offset-sidebar')}
            data-tour="nav:/chat"
            onClick={() => openAssign()}
            aria-current={onAssign ? 'page' : undefined}
          >
            <Plus />
            {intl.formatMessage({ id: chatEntryLabel('sidebar.newTask') })}
          </Button>
        )}
        {!employee && <SearchTrigger collapsed={collapsed} />}
      </SidebarHeader>

      <SidebarContent>
        {/* Flat top row — 新對話 then 儀表板, no group label. 收件匣 left this
            row on 2026-08-04 (D17); the footer bell is its entry point now. */}
        <SidebarGroup>
          <SidebarMenu>
            {dailyRow.map((item) => (
              <NavRow key={item.to} item={item} count={badgeFor(item.badge)} collapsed={collapsed} />
            ))}
          </SidebarMenu>
        </SidebarGroup>

        {/* Personal 主區 — the remaining flat rows. */}
        {isPersonal && personalRow.length > 0 && (
          <SidebarGroup>
            <SidebarMenu>
              {personalRow.map((item) => (
                <NavRow key={item.to} item={item} count={badgeFor(item.badge)} collapsed={collapsed} />
              ))}
            </SidebarMenu>
          </SidebarGroup>
        )}

        {/* Enterprise working area — 工作 / LIVE 員工 / 公司. Personal has no
            equivalent: its six primary surfaces are the flat row above. */}
        {!isPersonal && !employee && (
          <>
            <NavGroupSection
              label={navGroups[0].label}
              items={workItems}
              badgeFor={badgeFor}
              collapsed={collapsed}
              sectionCollapsed={isSectionCollapsed(navGroups[0].label)}
              onToggle={() => toggleSection(navGroups[0].label)}
            />

            <StaffZone
              collapsed={collapsed}
              sectionCollapsed={isSectionCollapsed('navGroup.staff')}
              onToggle={() => toggleSection('navGroup.staff')}
            />

            <NavGroupSection
              label={navGroups[1].label}
              items={companyItems}
              badgeFor={badgeFor}
              collapsed={collapsed}
              sectionCollapsed={isSectionCollapsed(navGroups[1].label)}
              onToggle={() => toggleSection(navGroups[1].label)}
            />

            {/* 組織 (1.76.0) — people-side pages; empty (and hidden) for an
                Employee, since every row is manager+. */}
            <NavGroupSection
              label={navGroups[3].label}
              items={orgItems}
              badgeFor={badgeFor}
              collapsed={collapsed}
              sectionCollapsed={isSectionCollapsed(navGroups[3].label)}
              onToggle={() => toggleSection(navGroups[3].label)}
            />
          </>
        )}

        {employee && <StaffZone employee collapsed={collapsed}
          sectionCollapsed={isSectionCollapsed('navGroup.staff')}
          onToggle={() => toggleSection('navGroup.staff')} />}

        {/* 對話紀錄 — moved out from under 新對話 to just above 設定 on
            2026-08-04 (WP18-B client feedback). Leading the rail with a list of
            past conversation titles pushed the six annotated primary surfaces
            (例行工作 → … → 世界) down the page; the history is a "what was I
            just doing" lookup, so it belongs at the tail of the working area.
            One slot, both editions — the rail order must not fork. */}
        <ConversationsZone
          collapsed={collapsed}
          sectionCollapsed={isSectionCollapsed('navGroup.conversations')}
          onToggle={() => toggleSection('navGroup.conversations')}
        />

        {/* 設定 closes the rail on both editions… */}
        {!employee && <NavGroupSection
          label={navGroups[2].label}
          items={settingsItems}
          badgeFor={badgeFor}
          collapsed={collapsed}
          sectionCollapsed={isSectionCollapsed(navGroups[2].label)}
          onToggle={() => toggleSection(navGroups[2].label)}
        />}

        {/* …with Personal's collapsed 進階 group after it (2026-07-29 IA). */}
        {isPersonal && (
          <NavGroupSection
            label={personalAdvancedGroup.label}
            items={advancedItems}
            badgeFor={badgeFor}
            collapsed={collapsed}
            sectionCollapsed={isSectionCollapsed(personalAdvancedGroup.label)}
            onToggle={() => toggleSection(personalAdvancedGroup.label)}
          />
        )}
      </SidebarContent>

      <SidebarFooter>
        {employee ? <EmployeeAccountMenu key={user?.id} collapsed={collapsed} /> : <>
        {!collapsed && <EditionCard />}
        <div className={cn('flex items-center gap-1', collapsed && 'flex-col')}>
          {/* Notification bell — the only standing entry to 收件匣 since it left
              the navigation (2026-08-04, D17). Quiet by default; it lights up
              and carries a count only when something actually needs a decision. */}
          <NavLink
            to={inboxEntry.to}
            data-tour={`nav:${inboxEntry.to}`}
            aria-label={inboxLabel}
            title={inboxLabel}
            className={({ isActive }) =>
              cn(
                'relative grid size-8 place-items-center rounded-md outline-none transition-colors hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-sidebar-ring',
                isActive
                  ? 'text-sidebar-accent-foreground'
                  : !inboxStale && inboxCount > 0
                    ? 'text-brand'
                    : 'text-muted-foreground',
              )
            }
          >
            <Bell className="size-4" />
            {(inboxStale || inboxCount > 0) && (
              <span className="absolute -right-0.5 -top-0.5 inline-flex min-w-4 items-center justify-center rounded-full bg-brand px-1 text-[9px] font-medium leading-none tabular-nums text-brand-foreground">
                {inboxStale ? '?' : inboxCount > 99 ? '99+' : inboxCount}
              </span>
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
        </>}
      </SidebarFooter>

      <SidebarRail />
    </>
  );
}
