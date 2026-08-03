import {
  Home,
  Inbox,
  MessageCircle,
  MessageCirclePlus,
  MessagesSquare,
  KanbanSquare,
  ListChecks,
  GitFork,
  CalendarClock,
  ChartGantt,
  BarChart3,
  Users,
  Users2,
  Network,
  Brain,
  Puzzle,
  LayoutGrid,
  Trophy,
  Radio,
  Plug,
  CreditCard,
  Cpu,
  Activity,
  Shield,
  Scale,
  KeyRound,
  Settings,
  FileText,
  Building2,
  Globe2,
  Import,
  Info,
  Store,
  ScrollText,
  Presentation,
  MonitorCog,
  FolderOpen,
  PawPrint,
  Package,
  LogIn,
  Download,
} from 'lucide-react';
import type { UserRole } from '@/stores/auth-store';
import type { Gated } from '@/lib/nav-visibility';

export type NavItem = Gated & {
  to: string;
  icon: typeof Home;
  /** i18n message id for the item label. */
  label: string;
  /**
   * i18n message id for a one-line description shown under the label in the
   * sidebar (and as a subtitle in the command palette). By convention this is
   * `${label}.desc`. Keeps the nav self-explanatory — no guessing from icons.
   */
  desc: string;
  /**
   * When set, the Sidebar renders a live count pill next to the item, sourced
   * from a store keyed by this name. `'inbox'` = the unified "needs me" count
   * (approvals + blocked + budget), tracked by `useApprovalsStore`.
   */
  badge?: 'inbox';
  /**
   * Breadcrumb label override. The sidebar row can be phrased as an action
   * ("新對話") while the page it lands on keeps its noun ("對話"). Defaults to
   * `label` when absent.
   */
  crumb?: string;
  /**
   * Side effect to run before navigating, for rows that are actions rather than
   * plain destinations. `'newConversation'` clears the chat view so 新對話 truly
   * starts a fresh thread — from the sidebar AND from ⌘K, not just one of them.
   * The previous conversation is preserved and stays resumable.
   */
  action?: 'newConversation';
};

export type NavGroup = {
  /** i18n message id for the group header. */
  label: string;
  items: NavItem[];
};

/**
 * Single source of truth for the "嘟嘟事務所" navigation, re-grouped for the
 * Multica app shell (WP0.4, spec §5.1). The Sidebar renders, top to bottom:
 *   1. `dailyItems` — flat, no group label (新對話 / 儀表板; 收件匣 moved to the
 *      footer bell on 2026-08-04, D17).
 *   1b. the `對話紀錄` zone — dynamic, sourced from `useConversationsStore`
 *      (2026-07-30 client feedback); on Personal it sits between the daily row
 *      and `personalPrimaryItems`, mirroring where 對話 used to be.
 *   2. the `工作` group (`navGroups[0]`) — collapsible GroupLabel.
 *   3. a LIVE 員工 zone — dynamic, sourced from the agents store, not static
 *      nav items (see AppSidebar). `staffEntry` is its "全部員工 →" link.
 *   4. the `公司` group (`navGroups[1]`) — collapsible.
 *   5. the `設定` group (`navGroups[2]`) — collapsible: 管理 + 關於.
 *
 * `navGroups` deliberately excludes the daily items so the flat daily row maps
 * 1:1 to its render block; the command palette and breadcrumb resolver fold
 * `dailyItems` back in (see `crumbsFor` + CommandPalette). `staffEntry` /
 * `manageEntry` are also referenced inside `navGroups` so ⌘K + breadcrumbs reach
 * them, and re-exported standalone for the live staff zone.
 *
 * Gating is per item (`minRole` / `enterprise` / `ownScope` / `operatorOnly`,
 * see `nav-visibility.ts`); a group hides entirely when the viewer can see none
 * of its items. Front-end gating is UX only — the gateway RPC layer is the real
 * gate (WP11, fail-closed).
 */

/**
 * Flat, always-first daily items (rendered with no section header).
 *
 * 2026-07-30 client feedback: the 對話 row became 「新對話」— an action that
 * starts a fresh thread — and the conversations it produces are listed by the
 * 對話紀錄 group rendered right below (see `ConversationsZone`). Same route;
 * only the phrasing and the pre-navigation side effect changed.
 */
export const dailyItems: NavItem[] = [
  // 2026-08-04 client feedback (D17): 新對話 leads the rail. It is the one row
  // people reach for every single session, and it was previously buried under
  // 儀表板 / 收件匣.
  {
    to: '/chat',
    icon: MessageCirclePlus,
    label: 'nav.newChat',
    desc: 'nav.newChat.desc',
    crumb: 'nav.chat',
    action: 'newConversation',
    ownScope: true,
  },
  { to: '/', icon: Home, label: 'nav.home', desc: 'nav.home.desc', ownScope: true },
  // 收件匣 left the standing navigation on 2026-08-04 (D17). The page and its
  // route stay — the entry point is now the footer notification bell, which
  // lights up only when something actually needs a decision. See
  // `inboxEntry` below (kept so ⌘K, breadcrumbs and deep links still resolve).
];

/**
 * 收件匣 — route + label kept alive after it left the standing navigation
 * (2026-08-04, D17). The sidebar footer bell is the entry point; this entry is
 * what lets ⌘K, the breadcrumb resolver and existing deep links keep working.
 */
export const inboxEntry: NavItem = {
  to: '/inbox',
  icon: Inbox,
  label: 'nav.inbox',
  desc: 'nav.inbox.desc',
  badge: 'inbox',
};

/**
 * 對話紀錄 — the full conversation list (2026-08-04, D17). The sidebar zone
 * shows only the newest few and links here for everything else.
 */
export const conversationsEntry: NavItem = {
  to: '/conversations',
  icon: MessagesSquare,
  label: 'nav.conversations',
  desc: 'nav.conversations.desc',
  ownScope: true,
};

/**
 * The 員工 roster entry — the "全部員工 →" link under the LIVE staff zone, the
 * lead item of the 公司 group, and the target the command palette exposes for
 * jumping to the roster.
 */
export const staffEntry: NavItem = {
  to: '/agents',
  icon: Users,
  label: 'nav.agents',
  desc: 'nav.agents.desc',
  ownScope: true,
  // 2026-08-04 client decision (D11) OVERTURNS the 2026-07-29 Personal IA call
  // that hid this page: Joanna's users do go looking for "my AI employees", and
  // hiding the roster made the product feel like it had lost them. The page is
  // visible on every edition again — only the LIVE staff zone stays
  // enterprise-only (see AppSidebar), because a one-person office does not need
  // a standing roster widget.
};

/**
 * The single Zone D entry — first item of the 設定 group. Visible from `manager`
 * up; each sub-item re-gates itself inside the ManageShell. `employee` never
 * sees the 管理 entry.
 */
export const manageEntry: NavItem = {
  to: '/manage',
  icon: Building2,
  label: 'nav.manage',
  desc: 'nav.manage.desc',
  minRole: 'manager',
};

export const navGroups: NavGroup[] = [
  {
    // 工作 — the work itself.
    label: 'navGroup.work',
    items: [
      // 任務看板 restored to the primary sidebar (2026-07-12 walkthrough): it's the
      // canonical work surface, so it leads the 工作 group. Still reachable from the
      // Home task-summary cards, the mobile ＋交辦 action, and ⌘K.
      { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks', desc: 'nav.tasks.desc', ownScope: true },
      // U4 co-edited plans — shared step lists between the user and an AI employee.
      { to: '/plans', icon: ListChecks, label: 'nav.plans', desc: 'nav.plans.desc', ownScope: true },
      // G12 run inspector — per-run transcripts (session turns + tool receipts).
      { to: '/runs', icon: ScrollText, label: 'nav.runs', desc: 'nav.runs.desc', ownScope: true },
      // G15 Live Canvas — agent-pushed HTML workspace, sandbox-rendered.
      { to: '/canvas', icon: Presentation, label: 'nav.canvas', desc: 'nav.canvas.desc', ownScope: true },
      // WP1.4 file panel — attachments an AI staff member received/produced.
      { to: '/files', icon: FolderOpen, label: 'nav.files', desc: 'nav.files.desc', ownScope: true },
      { to: '/routines', icon: CalendarClock, label: 'nav.routines', desc: 'nav.routines.desc', minRole: 'manager' },
      // G11 Work Timeline — company-level Gantt of every AI staff member's runs.
      { to: '/timeline', icon: ChartGantt, label: 'nav.timeline', desc: 'nav.timeline.desc', minRole: 'manager' },
      { to: '/reports', icon: BarChart3, label: 'nav.reports', desc: 'nav.reports.desc', minRole: 'manager' },
      // P4-3 — OS-native fleet report + settings (filesystem watch / frontmost
      // polling / footprint / proactive gate). All os.* RPCs are admin-gated
      // server-side (require_admin!); minRole mirrors that here.
      { to: '/os', icon: MonitorCog, label: 'nav.os', desc: 'nav.os.desc', minRole: 'admin' },
      // Progressive disclosure: hidden until the first fork ever runs — a
      // dormant RFC-26 surface shouldn't occupy nav space with a dead page.
      { to: '/forks', icon: GitFork, label: 'nav.forks', desc: 'nav.forks.desc', minRole: 'manager', requiresData: 'forks' },
    ],
  },
  {
    // 公司 — staff, team, world, memory (incl. the knowledge base), skills,
    // widgets, growth.
    label: 'navGroup.company',
    items: [
      staffEntry,
      { to: '/org', icon: Users2, label: 'nav.team', desc: 'nav.team.desc', minRole: 'manager', personalHidden: true },
      { to: '/world', icon: Globe2, label: 'nav.world', desc: 'nav.world.desc', ownScope: true },
      { to: '/memory', icon: Brain, label: 'nav.memory', desc: 'nav.memory.desc', ownScope: true },
      { to: '/skills', icon: Puzzle, label: 'nav.skills', desc: 'nav.skills.desc' },
      // 專家包 — install/manage bundled AI teams; experts.* RPCs are admin-only.
      { to: '/experts', icon: Package, label: 'nav.experts', desc: 'nav.experts.desc', minRole: 'admin' },
      // Widget 工坊 — custom dashboard cards (AI-built / HTML / shared).
      { to: '/widgets', icon: LayoutGrid, label: 'nav.widgets', desc: 'nav.widgets.desc' },
      { to: '/growth', icon: Trophy, label: 'nav.growth', desc: 'nav.growth.desc', ownScope: true },
      // 桌寵工作室 — photo → interactive desktop pet. Desktop app only
      // (2026-07-29): hidden in a plain browser instead of showing a stub page.
      { to: '/pet-studio', icon: PawPrint, label: 'nav.petStudio', desc: 'nav.petStudio.desc', desktopOnly: true },
    ],
  },
  {
    // 設定 — management shell entry + brand/about page.
    label: 'navGroup.settings',
    items: [
      manageEntry,
      // 關於 — brand info + fixed upstream-vendor block. Open to every user.
      { to: '/about', icon: Info, label: 'nav.about', desc: 'nav.about.desc' },
    ],
  },
];

// ── Personal edition IA (2026-07-29 client feedback) ────────────────────────
//
// Personal gets a deliberately minimal sidebar: the daily row plus a handful
// of primary surfaces, with every power-user page folded into a collapsed
// 「進階」 group at the bottom (below 設定, which also starts collapsed).
// Hidden entirely on Personal: AI 員工 roster (+ LIVE staff zone), 公司 org
// chart, 授權 (see `personalHidden` gates above / in `manageNav`). 桌寵工作室
// is desktop-app-only everywhere (`desktopOnly`). Enterprise keeps the
// original three-group layout untouched.
//
// Items are looked up by route so both layouts share the same NavItem objects
// (labels, icons, and role gates can never drift between editions).

const itemByPath = new Map<string, NavItem>(
  [...dailyItems, staffEntry, ...navGroups.flatMap((g) => g.items)].map((i) => [i.to, i]),
);

function pickItems(paths: string[]): NavItem[] {
  return paths.flatMap((p) => {
    const item = itemByPath.get(p);
    if (!item && import.meta.env.DEV) {
      throw new Error(`personal nav references unknown route: ${p}`);
    }
    return item ? [item] : [];
  });
}

/**
 * Personal 主區 — rendered flat right after the daily row (no group label).
 *
 * 2026-07-30: the knowledge base merged into 記憶, so `/memory` takes the slot
 * `/knowledge` used to hold here (and leaves 進階).
 */
export const personalPrimaryItems: NavItem[] = pickItems([
  // AI 員工 back on the primary rail for Personal too (2026-08-04, D11).
  '/agents',
  '/routines',
  '/world',
  '/skills',
  '/memory',
  '/pet-studio',
]);

/** Personal 進階 — collapsed-by-default group at the very bottom. */
export const personalAdvancedGroup: NavGroup = {
  label: 'navGroup.advanced',
  items: pickItems([
    '/tasks',
    '/plans',
    '/runs',
    '/canvas',
    '/files',
    '/timeline',
    '/reports',
    '/os',
    '/experts',
    '/widgets',
    '/growth',
    '/forks',
  ]),
};

/**
 * The collapsible group list for the current edition — used by the sidebar
 * and the command palette so both agree on grouping. Personal: 設定 then 進階
 * (primary items live in the flat row via `primaryItemsForEdition`).
 */
export function navGroupsForEdition(isPersonal: boolean): NavGroup[] {
  return isPersonal ? [navGroups[2], personalAdvancedGroup] : navGroups;
}

/** The flat, label-less top rows for the current edition. */
export function primaryItemsForEdition(isPersonal: boolean): NavItem[] {
  return isPersonal ? [...dailyItems, ...personalPrimaryItems] : dailyItems;
}

/**
 * Zone D subnav tree, rendered by ManageShell (§6.1). Collapses the former
 * 17-item navigation wall into one shell with a left subnav. Each entry keeps
 * its own role/enterprise gate — the shell hides items the viewer can't see.
 */
export const manageNav: NavItem[] = [
  { to: '/manage/channels', icon: Radio, label: 'manage.channels', desc: 'manage.channels.desc', minRole: 'admin' },
  { to: '/manage/integrations', icon: Plug, label: 'manage.integrations', desc: 'manage.integrations.desc', minRole: 'admin' },
  // 帳戶與登入 — lifted out of the billing page's second tab (2026-08-04, D16 /
  // D18). One-click CLI sign-in was the single most-asked-for thing nobody
  // could find; it now has its own top-level management entry.
  { to: '/manage/accounts', icon: LogIn, label: 'manage.accounts', desc: 'manage.accounts.desc', minRole: 'admin' },
  // 系統更新 — lifted out of the settings page's tab rail (2026-08-04, D18).
  { to: '/manage/updates', icon: Download, label: 'manage.updates', desc: 'manage.updates.desc', minRole: 'admin' },
  // 進階設定 — everything else, folded one level down. Nothing was removed;
  // `manageAdvancedNav` below is the full former list minus the four surfaces
  // promoted above.
  { to: '/manage/advanced', icon: Settings, label: 'manage.advanced', desc: 'manage.advanced.desc', minRole: 'manager' },
];

/**
 * The management surfaces folded under 進階設定 (2026-08-04, D18). These keep
 * their original routes — bookmarks, ⌘K and deep links are unaffected — they
 * simply no longer occupy a top-level rail slot. The `ManageShell` reveals them
 * as a sub-list whenever the viewer is inside this subtree.
 */
export const manageAdvancedNav: NavItem[] = [
  { to: '/manage/billing', icon: CreditCard, label: 'manage.billing', desc: 'manage.billing.desc', minRole: 'manager' },
  // 授權 hidden on Personal (2026-07-29 client feedback). The page stays
  // URL-reachable (`/manage/license`) and ⌘K still finds it on other editions.
  { to: '/manage/license', icon: KeyRound, label: 'manage.license', desc: 'manage.license.desc', minRole: 'manager', personalHidden: true },
  { to: '/manage/distributors', icon: Store, label: 'manage.distributors', desc: 'manage.distributors.desc', minRole: 'admin' },
  { to: '/manage/migrate', icon: Import, label: 'manage.migrate', desc: 'manage.migrate.desc', minRole: 'manager' },
  { to: '/manage/users', icon: Users, label: 'manage.users', desc: 'manage.users.desc', minRole: 'admin', enterprise: true },
  // Departments are an org grouping — an Enterprise concept. Personal is a
  // single-owner form factor with no departments, so this page (and the
  // department dropdowns that draw from it — agent-create dialog, skill-install
  // scope) are hidden in the Personal edition.
  { to: '/manage/departments', icon: Network, label: 'manage.departments', desc: 'manage.departments.desc', minRole: 'admin', enterprise: true },
  { to: '/manage/governance', icon: Scale, label: 'manage.governance', desc: 'manage.governance.desc', minRole: 'admin', enterprise: true },
  { to: '/manage/security', icon: Shield, label: 'manage.security', desc: 'manage.security.desc', minRole: 'admin' },
  { to: '/manage/reliability', icon: Activity, label: 'manage.reliability', desc: 'manage.reliability.desc', minRole: 'admin' },
  { to: '/manage/inference', icon: Cpu, label: 'manage.inference', desc: 'manage.inference.desc', minRole: 'admin' },
  { to: '/manage/logs', icon: FileText, label: 'manage.logs', desc: 'manage.logs.desc', minRole: 'manager' },
  { to: '/manage/system', icon: Settings, label: 'manage.system', desc: 'manage.system.desc', minRole: 'admin' },
];

/** Every management destination — the five rail entries plus what 進階設定 holds. */
export const allManageNav: NavItem[] = [...manageNav, ...manageAdvancedNav];

/**
 * Resolve the breadcrumb trail for a pathname (dashboard-redesign §8, paperclip
 * P6). Returns i18n message ids + optional link targets; the header translates
 * them. The ManageShell subtree gets a two-level trail (管理 / X); every other
 * page gets its single nav label. Daily / staff / manage items are folded back
 * in here since they live outside `navGroups`.
 */
export function crumbsFor(pathname: string): Array<{ labelId: string; to?: string }> {
  if (pathname.startsWith('/manage')) {
    const item = allManageNav.find((i) => pathname.startsWith(i.to));
    const advanced = manageAdvancedNav.some((i) => i.to === item?.to);
    return [
      { labelId: manageEntry.label, to: '/manage' },
      // Folded surfaces read as 管理 / 進階設定 / X so the trail matches the rail.
      ...(advanced ? [{ labelId: 'manage.advanced', to: '/manage/advanced' }] : []),
      ...(item ? [{ labelId: item.label }] : []),
    ];
  }
  const flat: NavItem[] = [...dailyItems, inboxEntry, conversationsEntry, staffEntry];
  for (const item of flat) {
    if (item.to === pathname || (item.to !== '/' && pathname.startsWith(item.to))) {
      return [{ labelId: item.crumb ?? item.label }];
    }
  }
  for (const group of navGroups) {
    const item = group.items.find(
      (i) => i.to === pathname || (i.to !== '/' && pathname.startsWith(i.to)),
    );
    if (item) return [{ labelId: item.crumb ?? item.label }];
  }
  return [];
}

/**
 * Zone A quick-access routes for the mobile bottom nav (§4.3). The `+ 交辦任務`
 * center action is injected by MobileBottomNav itself (and links to the task
 * board's create intent). Side slots: 儀表板 / 對話 | ＋ | 對話紀錄 / 任務.
 *
 * 2026-08-04 (D17): 收件匣 gave up its slot to 對話紀錄. Its count now shows as a
 * bell in the mobile top bar (`MainLayout`), which only appears when something
 * is pending — so no bottom-bar item carries a badge any more.
 */
export const mobileNavItems: NavItem[] = [
  { to: '/', icon: Home, label: 'nav.home', desc: 'nav.home.desc' },
  { to: '/chat', icon: MessageCircle, label: 'nav.chat', desc: 'nav.chat.desc' },
  // 2026-08-04 (D17): 收件匣 left the standing navigation; 對話紀錄 takes the
  // slot so the full conversation list is one tap away on a phone too. The
  // inbox is still reachable from the drawer's notification bell.
  { to: '/conversations', icon: MessagesSquare, label: 'nav.conversations', desc: 'nav.conversations.desc' },
  // Task board restored as a primary nav item (R2); keeps the two side groups
  // balanced 2/2 around the centre ＋交辦 action.
  { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks', desc: 'nav.tasks.desc' },
];

export type { UserRole };
