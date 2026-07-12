import {
  Home,
  Inbox,
  MessageCircle,
  KanbanSquare,
  ListChecks,
  GitFork,
  CalendarClock,
  ChartGantt,
  BarChart3,
  Users,
  Users2,
  Brain,
  Puzzle,
  BookOpen,
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
};

export type NavGroup = {
  /** i18n message id for the group header. */
  label: string;
  items: NavItem[];
};

/**
 * Single source of truth for the "嘟嘟事務所" navigation (dashboard-redesign-v2
 * §4.2). The Sidebar renders, top to bottom:
 *   1. `dailyItems` — flat, no header (Home / Inbox / Chat).
 *   2. the `工作` group (`navGroups[0]`) — collapsible.
 *   3. a LIVE 員工 zone — dynamic, sourced from the agents store, not static
 *      nav items (see Sidebar). `staffEntry` is its "全部員工 →" link.
 *   4. the `公司` group (`navGroups[1]`) — collapsible — plus `manageEntry`.
 *
 * `navGroups` deliberately excludes the daily + staff + manage items so the
 * two collapsible sections map 1:1 to their render blocks; the command palette
 * and breadcrumb resolver fold `dailyItems` / `staffEntry` / `manageEntry` back
 * in (see `crumbsFor` + CommandPalette).
 *
 * Gating is per item (`minRole` / `enterprise` / `ownScope` / `operatorOnly`,
 * see `nav-visibility.ts`); a group hides entirely when the viewer can see none
 * of its items. Front-end gating is UX only — the gateway RPC layer is the real
 * gate (WP11, fail-closed).
 */

/** Flat, always-first daily items (rendered with no section header). */
export const dailyItems: NavItem[] = [
  { to: '/', icon: Home, label: 'nav.home', desc: 'nav.home.desc', ownScope: true },
  { to: '/inbox', icon: Inbox, label: 'nav.inbox', desc: 'nav.inbox.desc', badge: 'inbox' },
  { to: '/chat', icon: MessageCircle, label: 'nav.chat', desc: 'nav.chat.desc', ownScope: true },
];

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
      { to: '/routines', icon: CalendarClock, label: 'nav.routines', desc: 'nav.routines.desc', minRole: 'manager' },
      // G11 Work Timeline — company-level Gantt of every AI staff member's runs.
      { to: '/timeline', icon: ChartGantt, label: 'nav.timeline', desc: 'nav.timeline.desc', minRole: 'manager' },
      { to: '/reports', icon: BarChart3, label: 'nav.reports', desc: 'nav.reports.desc', minRole: 'manager' },
      // Progressive disclosure: hidden until the first fork ever runs — a
      // dormant RFC-26 surface shouldn't occupy nav space with a dead page.
      { to: '/forks', icon: GitFork, label: 'nav.forks', desc: 'nav.forks.desc', minRole: 'manager', requiresData: 'forks' },
    ],
  },
  {
    // 公司 — team, memory, skills, knowledge, growth (管理 appended separately).
    label: 'navGroup.company',
    items: [
      { to: '/org', icon: Users2, label: 'nav.team', desc: 'nav.team.desc', minRole: 'manager' },
      { to: '/world', icon: Globe2, label: 'nav.world', desc: 'nav.world.desc', ownScope: true },
      { to: '/memory', icon: Brain, label: 'nav.memory', desc: 'nav.memory.desc', ownScope: true },
      { to: '/skills', icon: Puzzle, label: 'nav.skills', desc: 'nav.skills.desc' },
      { to: '/knowledge', icon: BookOpen, label: 'nav.knowledge', desc: 'nav.knowledge.desc' },
      { to: '/growth', icon: Trophy, label: 'nav.growth', desc: 'nav.growth.desc', ownScope: true },
      // 關於 — brand info + fixed upstream-vendor block. Open to every user.
      { to: '/about', icon: Info, label: 'nav.about', desc: 'nav.about.desc' },
    ],
  },
];

/**
 * The 員工 roster entry — the "全部員工 →" link under the LIVE staff zone, and
 * the target the command palette exposes for jumping to the roster.
 */
export const staffEntry: NavItem = {
  to: '/agents',
  icon: Users,
  label: 'nav.agents',
  desc: 'nav.agents.desc',
  ownScope: true,
};

/**
 * The single Zone D entry shown in the main sidebar (last item of the 公司
 * section). Visible from `manager` up; each sub-item re-gates itself inside the
 * ManageShell. `employee` never sees the 管理 entry.
 */
export const manageEntry: NavItem = {
  to: '/manage',
  icon: Building2,
  label: 'nav.manage',
  desc: 'nav.manage.desc',
  minRole: 'manager',
};

/**
 * Zone D subnav tree, rendered by ManageShell (§6.1). Collapses the former
 * 17-item navigation wall into one shell with a left subnav. Each entry keeps
 * its own role/enterprise gate — the shell hides items the viewer can't see.
 */
export const manageNav: NavItem[] = [
  { to: '/manage/channels', icon: Radio, label: 'manage.channels', desc: 'manage.channels.desc', minRole: 'admin' },
  { to: '/manage/integrations', icon: Plug, label: 'manage.integrations', desc: 'manage.integrations.desc', minRole: 'admin' },
  { to: '/manage/billing', icon: CreditCard, label: 'manage.billing', desc: 'manage.billing.desc', minRole: 'manager' },
  { to: '/manage/inference', icon: Cpu, label: 'manage.inference', desc: 'manage.inference.desc', minRole: 'admin' },
  { to: '/manage/reliability', icon: Activity, label: 'manage.reliability', desc: 'manage.reliability.desc', minRole: 'admin' },
  { to: '/manage/security', icon: Shield, label: 'manage.security', desc: 'manage.security.desc', minRole: 'admin' },
  { to: '/manage/governance', icon: Scale, label: 'manage.governance', desc: 'manage.governance.desc', minRole: 'admin', enterprise: true },
  { to: '/manage/users', icon: Users, label: 'manage.users', desc: 'manage.users.desc', minRole: 'admin', enterprise: true },
  { to: '/manage/license', icon: KeyRound, label: 'manage.license', desc: 'manage.license.desc', minRole: 'manager' },
  { to: '/manage/distributors', icon: Store, label: 'manage.distributors', desc: 'manage.distributors.desc', minRole: 'admin' },
  { to: '/manage/migrate', icon: Import, label: 'manage.migrate', desc: 'manage.migrate.desc', minRole: 'manager' },
  { to: '/manage/logs', icon: FileText, label: 'manage.logs', desc: 'manage.logs.desc', minRole: 'manager' },
  { to: '/manage/system', icon: Settings, label: 'manage.system', desc: 'manage.system.desc', minRole: 'admin' },
];

/**
 * Resolve the breadcrumb trail for a pathname (dashboard-redesign §8, paperclip
 * P6). Returns i18n message ids + optional link targets; the header translates
 * them. The ManageShell subtree gets a two-level trail (管理 / X); every other
 * page gets its single nav label. Daily / staff / manage items are folded back
 * in here since they live outside `navGroups`.
 */
export function crumbsFor(pathname: string): Array<{ labelId: string; to?: string }> {
  if (pathname.startsWith('/manage')) {
    const item = manageNav.find((i) => pathname.startsWith(i.to));
    return [
      { labelId: manageEntry.label, to: '/manage' },
      ...(item ? [{ labelId: item.label }] : []),
    ];
  }
  const flat: NavItem[] = [...dailyItems, staffEntry];
  for (const item of flat) {
    if (item.to === pathname || (item.to !== '/' && pathname.startsWith(item.to))) {
      return [{ labelId: item.label }];
    }
  }
  for (const group of navGroups) {
    const item = group.items.find(
      (i) => i.to === pathname || (i.to !== '/' && pathname.startsWith(i.to)),
    );
    if (item) return [{ labelId: item.label }];
  }
  return [];
}

/**
 * Zone A quick-access routes for the mobile bottom nav (§4.3). The `+ 交辦任務`
 * center action is injected by MobileBottomNav itself (and links to the task
 * board's create intent). Side slots: 首頁 / 收件匣 | ＋ | 對話. The task board
 * now sits in the desktop sidebar's 工作 group; on mobile it stays reachable via
 * the ＋交辦 action, Home task cards, ⌘K, and its URL (the compact 4-slot bottom
 * bar is kept — 對話 is the primary mobile entry).
 */
export const mobileNavItems: NavItem[] = [
  { to: '/', icon: Home, label: 'nav.home', desc: 'nav.home.desc' },
  { to: '/inbox', icon: Inbox, label: 'nav.inbox', desc: 'nav.inbox.desc', badge: 'inbox' },
  { to: '/chat', icon: MessageCircle, label: 'nav.chat', desc: 'nav.chat.desc' },
  // Task board restored as a primary nav item (R2); keeps the two side groups
  // balanced 2/2 around the centre ＋交辦 action.
  { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks', desc: 'nav.tasks.desc' },
];

export type { UserRole };
