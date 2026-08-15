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
  HardDriveDownload,
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
  Images,
  Mail,
  LogIn,
  Download,
  Crosshair,
  Radar as RadarIcon,
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
  /**
   * CONVENTION (2026-08-14, user directive): every NEW feature page gets
   * `newIn: '<the release it ships in>'` when its nav item is added —
   * regardless of whether the row lands in the 一般 or 進階 layer. The
   * sidebar renders a 「新功能」 chip while the running version is at or
   * below that release's major.minor ([`isNewFeature`]) and drops it
   * automatically on the next minor — no manual cleanup pass needed, stale
   * `newIn` values are inert. Do NOT remove old `newIn` fields; they
   * self-expire.
   */
  newIn?: string;
};

/**
 * Whether a `newIn`-tagged nav item should still wear the 「新功能」 chip:
 * true while the running version's (major, minor) is ≤ the shipping
 * release's. An unknown running version keeps the chip (a spurious chip is
 * harmless; a missing one defeats the convention). Patch releases never
 * expire a chip — only the next minor does.
 */
export function isNewFeature(newIn: string | undefined, currentVersion: string | null): boolean {
  if (!newIn) return false;
  const parse = (v: string): [number, number] | null => {
    const m = v.trim().replace(/^v/, '').match(/^(\d+)\.(\d+)/);
    return m ? [Number(m[1]), Number(m[2])] : null;
  };
  const target = parse(newIn);
  if (!target) return false;
  const current = currentVersion ? parse(currentVersion) : null;
  if (!current) return true;
  return current[0] < target[0] || (current[0] === target[0] && current[1] <= target[1]);
}

export type NavGroup = {
  /** i18n message id for the group header. */
  label: string;
  items: NavItem[];
};

/**
 * Single source of truth for the "嘟嘟事務所" navigation, re-grouped for the
 * Multica app shell (WP0.4, spec §5.1). The Sidebar renders, top to bottom:
 *   1. `dailyItems` — flat, no group label (新對話 / 儀表板; 收件匣 moved to the
 *      footer bell on 2026-08-04, D17). On Personal, `personalPrimaryItems`
 *      follows immediately in the same flat block.
 *   2. the `工作` group (`navGroups[0]`) — collapsible GroupLabel.
 *   3. a LIVE 員工 zone — dynamic, sourced from the agents store, not static
 *      nav items (see AppSidebar). `staffEntry` is its "全部員工 →" link.
 *   4. the `公司` group (`navGroups[1]`) — collapsible.
 *      (2-4 are Enterprise-only; Personal's equivalents are the flat row.)
 *   5. the `對話紀錄` zone — dynamic, sourced from `useConversationsStore`.
 *      Moved here from directly under 新對話 on 2026-08-04 (WP18-B): the
 *      annotated primary order 新對話 → 例行工作 → 技能庫 → 記憶 → AI 員工 →
 *      世界 must not be interrupted by a list of past conversation titles.
 *      Identical slot on both editions.
 *   6. the `設定` group (`navGroups[2]`) — collapsible: 管理 + 關於; then
 *      Personal's collapsed `進階` group.
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
 * ── 一般層 / 進階層：the ordering law for everything in this file ────────────
 * (WP-NAV, 2026-08-12; `12-ia-redesign-blueprint.md` §2 over the frequency ×
 * importance matrix in `10-ia-scatter-audit.md` §4.)
 *
 * The navigation has exactly TWO disclosure layers — the 2026-08 IA audit found
 * the four-layer status quo to be the root cause of "I can't find it":
 *
 *   一般層 (open by default) = 每日 rail + 工作 + 公司.
 *     Everything a person touches in an ordinary week lives here, in
 *     frequency order, phrased in plain words. Low-frequency rows are not
 *     removed from these groups — they sink to the tail, ahead of nothing.
 *
 *   進階層 (folded away) = 管理 → 進階設定 (`manageAdvancedNav`), plus the
 *     Personal edition's collapsed 進階 group (`personalAdvancedGroup`).
 *     Ordered 低頻・高重要 first (money, then access control), 低頻・低重要
 *     last (logs / reliability / models / migration), catch-all 設定 dead last.
 *
 * Two invariants outrank the matrix and must survive any future reshuffle:
 *   1. 2026-08-04 WP14 client order, both editions:
 *      新對話 → 例行工作 → 技能庫 → 記憶 → AI 員工 → 世界, 任務看板 demoted.
 *   2. 2026-08-04 WP14 money order inside the 進階層:
 *      帳務 → 授權 → 經銷 before every operational row; 設定 last.
 *
 * "Demoting a row into 進階 decides that 95% of users will live with its
 * default forever" — so a demotion is only honest when the default is right.
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
      // 例行工作 leads the 工作 group (2026-08-04 WP14 client annotation): the
      // agreed priority order across BOTH editions is 新對話 → 例行工作 → 技能庫
      // → 記憶 → AI 員工 → 世界, and 任務看板 is explicitly demoted (folded into
      // 進階 on Personal; last row of this group on Enterprise, where no 進階
      // group exists). Nothing was removed — the board keeps its route, the Home
      // task-summary cards, the mobile ＋交辦 action, and ⌘K.
      // D7 (09-edition-split-features.md §4): `minRole: 'manager'` used to gate
      // this row, which was never a real access-control question on Personal
      // (single owner = always admin) but reads oddly for a future `employee`
      // role — a cron schedule for one's own agent is the same "own scope" as
      // /plans (no gate) and /runs (no gate), so it drops to `ownScope` here.
      // — daily rows (一般層, frequency order).
      { to: '/routines', icon: CalendarClock, label: 'nav.routines', desc: 'nav.routines.desc', ownScope: true },
      // U4 co-edited plans — shared step lists between the user and an AI employee.
      { to: '/plans', icon: ListChecks, label: 'nav.plans', desc: 'nav.plans.desc', ownScope: true },
      // Goal-loop console (2026-08-14) — assign autonomous goals and intervene
      // at the human nodes (kickoff approvals, needs_human escalations).
      { to: '/goals', icon: Crosshair, label: 'nav.goals', desc: 'nav.goals.desc', ownScope: true, newIn: '1.58.0' },
      // G12 run inspector — per-run transcripts (session turns + tool receipts).
      { to: '/runs', icon: ScrollText, label: 'nav.runs', desc: 'nav.runs.desc', ownScope: true },
      // G15 Live Canvas — agent-pushed HTML workspace, sandbox-rendered.
      { to: '/canvas', icon: Presentation, label: 'nav.canvas', desc: 'nav.canvas.desc', ownScope: true },
      // WP1.4 file panel — attachments an AI staff member received/produced.
      { to: '/files', icon: FolderOpen, label: 'nav.files', desc: 'nav.files.desc', ownScope: true },
      // 信箱 (Agent Mail, P2-d, 2026-08-15) — the non-real-time channel: mail
      // arrives here, and every outgoing reply waits for a human 確認. Sits
      // next to 檔案 because both are "material an AI employee received or
      // produced". Manager-gated to match `mail.*`, which is on the same tier
      // as the approval centre (real correspondence + a real-world send).
      { to: '/mail', icon: Mail, label: 'nav.mail', desc: 'nav.mail.desc', minRole: 'manager', newIn: '1.60.0' },
      // — oversight rows: read weekly, not daily (WP-NAV frequency order).
      // G11 Work Timeline — company-level Gantt of every AI staff member's runs.
      { to: '/timeline', icon: ChartGantt, label: 'nav.timeline', desc: 'nav.timeline.desc', minRole: 'manager' },
      { to: '/reports', icon: BarChart3, label: 'nav.reports', desc: 'nav.reports.desc', minRole: 'manager' },
      // LLM→LWM loop view (2026-08-14) — predict→act→observe→score per task,
      // with the query-time skill verdict. RPCs are manager-gated.
      { to: '/foresight', icon: RadarIcon, label: 'nav.foresight', desc: 'nav.foresight.desc', minRole: 'manager', newIn: '1.58.0' },
      // — occasional rows: 低頻, and the only place in the 一般層 where a
      // machine-shaped word (OS) still shows. WP-NAV (2026-08-12) sank them
      // below the oversight pair and put 分支決戰 first of the two: it is
      // 低頻・高重要 (an irreversible pick between branches, matrix §4) while
      // OS is a 低頻 fleet report. Neither leaves the group — Enterprise has no
      // 進階 group to fold them into, so "tail of 工作" IS the demotion.
      // Progressive disclosure: hidden until the first fork ever runs — a
      // dormant RFC-26 surface shouldn't occupy nav space with a dead page.
      { to: '/forks', icon: GitFork, label: 'nav.forks', desc: 'nav.forks.desc', minRole: 'manager', requiresData: 'forks' },
      // P4-3 — OS-native fleet report + settings (filesystem watch / frontmost
      // polling / footprint / proactive gate). All os.* RPCs are admin-gated
      // server-side (require_admin!); minRole mirrors that here.
      { to: '/os', icon: MonitorCog, label: 'nav.os', desc: 'nav.os.desc', minRole: 'admin' },
      // 任務看板 — demoted to the tail of the group (2026-08-04 WP14). Enterprise
      // has no 進階 group to fold it into, so "least prominent slot" is the
      // closest equivalent to the Personal treatment.
      { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks', desc: 'nav.tasks.desc', ownScope: true },
    ],
  },
  {
    // 公司 — staff, team, world, memory (incl. the knowledge base), skills,
    // widgets, growth.
    label: 'navGroup.company',
    items: [
      // Ordered 技能庫 → 記憶 → AI 員工 → 世界 to match the Personal primary rail
      // (2026-08-04 WP14): the two editions must not present the same six
      // surfaces in two different orders.
      { to: '/skills', icon: Puzzle, label: 'nav.skills', desc: 'nav.skills.desc' },
      { to: '/memory', icon: Brain, label: 'nav.memory', desc: 'nav.memory.desc', ownScope: true },
      staffEntry,
      { to: '/world', icon: Globe2, label: 'nav.world', desc: 'nav.world.desc', ownScope: true },
      // 成長 lifted above the occasional rows (WP-NAV, 2026-08-12): the matrix
      // (10-ia-scatter-audit.md §4) grades it 高頻・低重要 — people open it
      // often even though nothing depends on it — so it belongs with the daily
      // surfaces rather than buried under three 低頻 rows. It stays BELOW the
      // client-annotated four (技能庫 → 記憶 → AI 員工 → 世界), which nothing
      // may push down.
      { to: '/growth', icon: Trophy, label: 'nav.growth', desc: 'nav.growth.desc', ownScope: true },
      // — 低頻 tail of 公司, in importance order.
      // D6 (09-edition-split-features.md §4): this used to be `personalHidden`
      // — reasoning it drew the agent `reports_to` tree, not a chart of human
      // reporting lines, so a Personal instance "shouldn't" need it. The
      // counter-argument that won: a 1-2-agent org chart is genuinely an empty
      // diagram no matter the edition, so `requiresData: 'org'` (progressive
      // disclosure, same mechanism as `/forks`) is the honest fix — it shows
      // up once there is actually something to draw, on EVERY edition.
      { to: '/org', icon: Users2, label: 'nav.team', desc: 'nav.team.desc', minRole: 'manager', requiresData: 'org' },
      // AI 團隊（原「專家包」，P2-a-nav 2026-08-15）— install/manage bundled AI
      // teams. Previously `minRole: 'admin'` kept 22 ready-made industry
      // teams invisible to every non-admin viewer (design doc walkthrough 5,
      // `DESIGN-dashboard-ux-workbuddy-2026-08.md` §1 走查5) — dropped so it
      // sits in the 一般層 like its 公司-group peers (`/skills`, `/memory`,
      // `/widgets`, …). The install action's own permission gate
      // (`experts.install_builtin` etc.) is untouched: seeing the card is not
      // the same as being allowed to install it.
      { to: '/experts', icon: Package, label: 'nav.experts', desc: 'nav.experts.desc' },
      // 靈感畫廊 (P2-b, curated MVP, 2026-08-15) — "做過的好案例" showcase, one
      // click into a prefilled 交辦 panel. Sits right after 「AI 團隊」: the two
      // are a pair (install a team → see what it can do), and both are the
      // 低頻 tail of this group.
      { to: '/gallery', icon: Images, label: 'nav.gallery', desc: 'nav.gallery.desc', newIn: '1.60.0' },
      // Widget 工坊 — custom dashboard cards (AI-built / HTML / shared).
      { to: '/widgets', icon: LayoutGrid, label: 'nav.widgets', desc: 'nav.widgets.desc' },
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
// Hidden entirely on Personal: AI 員工 LIVE staff zone, 授權 (see
// `personalHidden` gates above / in `manageNav`). 公司 org chart is no longer
// unconditionally hidden (D6) — it lives in 進階 and progressively discloses
// itself once the roster is ≥3 (`requiresData: 'org'`). 桌寵工作室 is
// desktop-app-only everywhere (`desktopOnly`). Enterprise keeps the original
// three-group layout untouched.
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
  // 2026-08-04 WP14 client annotation fixes this order outright:
  //   1 新對話 · 2 例行工作 · 3 技能庫 · 4 記憶 · 5 AI 員工 · 6 世界
  // 新對話 lives in `dailyItems` (rendered above this row), so rows 2-6 are
  // exactly what follows. 桌寵工作室 stays at the tail — it is desktop-only and
  // was not part of the annotated list.
  '/routines',
  // 2026-08-14 user directive: the two new loop consoles live on the primary
  // rail, not folded into 進階 — assigning/monitoring goals is daily work.
  '/goals',
  '/foresight',
  '/skills',
  '/memory',
  // AI 員工 back on the primary rail for Personal too (2026-08-04, D11).
  '/agents',
  '/world',
  // AI 團隊（原「專家包」，P2-a-nav 2026-08-15）— promoted out of 進階 (see
  // `personalAdvancedGroup` below, where the same route is now REMOVED —
  // leaving it in both places would render the row twice). 22 ready-made
  // industry teams were previously undiscoverable on Personal: admin-gated
  // AND buried at the bottom of a collapsed group (design doc walkthrough 5).
  '/experts',
  // 靈感畫廊 (P2-b, curated MVP, 2026-08-15) — right after 「AI 團隊」 on Personal
  // too, same pairing rationale as the Enterprise 公司 group above.
  '/gallery',
  '/pet-studio',
]);

/**
 * Personal 進階 — collapsed-by-default group at the very bottom, and the
 * Personal edition's half of the 進階層.
 *
 * Order (WP-NAV, 2026-08-12) mirrors the Enterprise 工作 → 公司 reading order
 * exactly, so the same surfaces never appear in two different sequences across
 * editions: the work cluster first (任務看板 leading it per WP14), then the
 * company cluster (成長 → 組織架構 → Widget 工坊). Within the work
 * cluster the same frequency slope applies — daily surfaces, oversight pair,
 * then the two occasional rows (分支決戰 → OS).
 *
 * AI 團隊（原「專家包」）left this group for `personalPrimaryItems`
 * (P2-a-nav, 2026-08-15) — 22 ready-made industry teams should not sit
 * behind a collapsed 進階 section.
 */
export const personalAdvancedGroup: NavGroup = {
  label: 'navGroup.advanced',
  items: pickItems([
    // 任務看板 folded in here on 2026-08-04 (WP14): still one click away, no
    // longer competing with the six surfaces people actually open daily.
    '/tasks',
    '/plans',
    '/runs',
    '/canvas',
    '/files',
    // 信箱 (Agent Mail, P2-d) — same slot as the Enterprise 工作 group (right
    // after 檔案). It follows the `/timeline` + `/reports` precedent rather
    // than the primary rail: manager-gated surfaces live in 進階 on Personal,
    // and the 2026-08-04 client-annotated primary order (例行工作 → 技能庫 →
    // 記憶 → AI 員工 → 世界) is fixed and must not be reshuffled.
    '/mail',
    '/timeline',
    '/reports',
    '/forks',
    '/os',
    // Company cluster. 成長 leads it (高頻・低重要, matrix §4) — same slot it
    // holds in the Enterprise 公司 group. D6: the org chart is folded here
    // rather than on the primary rail; it is a progressive-disclosure surface
    // (`requiresData: 'org'`), not a daily one.
    // `/experts` (AI 團隊) moved OUT of this group to `personalPrimaryItems`
    // (P2-a-nav 2026-08-15) — do not add it back here, it would render twice.
    '/growth',
    '/org',
    '/widgets',
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
 *
 * This list IS the 進階層 (see the layering note at the top of this file), so
 * WP-NAV (2026-08-12) sorted it 低頻・高重要 → 低頻・低重要 per
 * `12-ia-redesign-blueprint.md` §2 and the matrix in `10-ia-scatter-audit.md`
 * §4, in four runs:
 *
 *   1. 錢 — 帳務 → 授權 → 經銷. Order invariant (2026-08-04 WP14 client
 *      annotation): "這個要花多少錢" may never sit below an operational row,
 *      and 設定 stays LAST. 經銷 is 低頻・低重要 on the matrix but the client
 *      pinned it to the money run; the annotation wins.
 *   2. 存取與安全 — 安全 → 治理 → 成員 → 部門. All 低頻・高重要: one switch
 *      here changes who may do what, so they lead the operational rows.
 *   3. 維運 — 日誌 → 可靠性 → 模型用量 → 資料搬家, the blueprint's own
 *      低頻・低重要 sequence. 可靠性 and 模型用量 stay ADJACENT on purpose:
 *      the fallback rate is observed on one page and caused by a threshold
 *      edited on the other (10-ia-scatter-audit.md §2-14), so splitting them
 *      is what made that pair hard to use.
 *   4. 設定 — the catch-all, dead last.
 *
 * 帳戶與登入 (`/manage/accounts`) is NOT in this list even though it is
 * 低頻・高重要: D16/D18 promoted it to the five-row rail above. Nothing to
 * re-sort — it is already ahead of everything here.
 *
 * 可靠性 carries `personalHidden` — an SRE-style fleet report that a one-person
 * office never opens, and which made the advanced list read as a wall of
 * jargon. The route stays URL-reachable and every other edition keeps it.
 *
 * 安全 / 日誌 dropped their `personalHidden` gate (D9,
 * 09-edition-split-features.md §4): a single operator still needs an
 * emergency brake and a way to see what happened, so both are visible here —
 * same "folded under 進階設定" treatment as everything else in this list, not
 * hidden. `SecurityPage` itself splits its content by edition (kill switch +
 * audit log stay, in a page-internal collapsed section; the RBAC / credential
 * proxy / mount guard cards — organisation-scale views with no single-owner
 * counterpart — stay hidden there).
 */
export const manageAdvancedNav: NavItem[] = [
  // ── 1. 錢（客戶排序鐵律：帳務 → 授權 → 經銷，其餘一律排在後面）
  { to: '/manage/billing', icon: CreditCard, label: 'manage.billing', desc: 'manage.billing.desc', minRole: 'manager' },
  // 授權 hidden on Personal (2026-07-29 client feedback). The page stays
  // URL-reachable (`/manage/license`) and ⌘K still finds it on other editions.
  { to: '/manage/license', icon: KeyRound, label: 'manage.license', desc: 'manage.license.desc', minRole: 'manager', personalHidden: true },
  // 經銷商管理 signs real, machine-fingerprint-bound OEM licences — an
  // Enterprise-only capability that a Personal instance has no counterpart for.
  // It was the one such surface missing its gate (10-ia-scatter-audit D8), so a
  // Personal instance could see the licence-issuing console. Bug fix, not a
  // policy change.
  { to: '/manage/distributors', icon: Store, label: 'manage.distributors', desc: 'manage.distributors.desc', minRole: 'admin', enterprise: true },
  // ── 2. 存取與安全（低頻・高重要：改一格就改變「誰可以做什麼」）
  // D9: no longer `personalHidden` — `SecurityPage` itself hides the
  // organisation-scale content on Personal (see the block comment above).
  { to: '/manage/security', icon: Shield, label: 'manage.security', desc: 'manage.security.desc', minRole: 'admin' },
  { to: '/manage/governance', icon: Scale, label: 'manage.governance', desc: 'manage.governance.desc', minRole: 'admin', enterprise: true },
  { to: '/manage/users', icon: Users, label: 'manage.users', desc: 'manage.users.desc', minRole: 'admin', enterprise: true },
  // Departments are an org grouping — an Enterprise concept. Personal is a
  // single-owner form factor with no departments, so this page (and the
  // department dropdowns that draw from it — agent-create dialog, skill-install
  // scope) are hidden in the Personal edition.
  { to: '/manage/departments', icon: Network, label: 'manage.departments', desc: 'manage.departments.desc', minRole: 'admin', enterprise: true },
  // ── 3. 維運（低頻・低重要：日誌 → 可靠性 → 模型用量 → 資料搬家）
  // D9: no longer `personalHidden` — a single operator needs to see what
  // happened too; it stays folded under 進階設定 same as every other row here.
  { to: '/manage/logs', icon: FileText, label: 'manage.logs', desc: 'manage.logs.desc', minRole: 'manager' },
  { to: '/manage/reliability', icon: Activity, label: 'manage.reliability', desc: 'manage.reliability.desc', minRole: 'admin', personalHidden: true },
  { to: '/manage/inference', icon: Cpu, label: 'manage.inference', desc: 'manage.inference.desc', minRole: 'admin' },
  // 本地模型市集 — intent + hardware-fit HF picker with one-click install
  // (design: DESIGN-local-model-marketplace-2026-08-13).
  { to: '/manage/local-models', icon: HardDriveDownload, label: 'manage.localModels', desc: 'manage.localModels.desc', minRole: 'admin' },
  // 資料搬家 is a one-shot wizard — the least-often-opened row that still is
  // not the catch-all settings page.
  { to: '/manage/migrate', icon: Import, label: 'manage.migrate', desc: 'manage.migrate.desc', minRole: 'manager' },
  // ── 4. 設定 last（2026-08-04 鐵律）
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
