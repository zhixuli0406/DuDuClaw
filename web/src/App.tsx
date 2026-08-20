import { useEffect, lazy, Suspense } from 'react';
import { Routes, Route, Navigate, useNavigate } from 'react-router';
import { onPetOpenStudio } from './lib/pet';
import { client } from './lib/ws-client';
import { handleDashboardNavigate } from './lib/dashboard-navigate';
import { toast } from './lib/toast';
import { MainLayout } from './components/layout/MainLayout';
import { ManageShell } from './components/layout/ManageShell';
import { AuthGuard, RoleGuard, EditionGuard } from './components/AuthGuard';
import { FirstRunGate } from './components/FirstRunGate';
import { LoginPage } from './pages/LoginPage';
import { useConnectionStore } from './stores/connection-store';
import { useAuthStore } from './stores/auth-store';
import { ApprovalModal } from './components/ApprovalModal';
import { AppRouteRedirect, LegacyRouteRedirect } from './apps/AppRouteRedirect';
import { useIsAppliance } from './hooks/useIsAppliance';

// Code-splitting: every authenticated page is lazy-loaded so heavy, page-only
// dependencies (d3 for WikiGraph/OrgChart, large forms) land in their own route
// chunk instead of the main bundle. LoginPage stays eager for instant first paint.
// Named exports are adapted to lazy()'s default-export contract inline.
const lazyPage = <K extends string>(loader: () => Promise<Record<K, React.ComponentType>>, key: K) =>
  lazy(() => loader().then((m) => ({ default: m[key] })));

const HomePage = lazyPage(() => import('./pages/HomePage'), 'HomePage');
const InboxPage = lazyPage(() => import('./pages/InboxPage'), 'InboxPage');
const RoutinesPage = lazyPage(() => import('./pages/RoutinesPage'), 'RoutinesPage');
const TimelinePage = lazyPage(() => import('./pages/TimelinePage'), 'TimelinePage');
const RunsPage = lazyPage(() => import('./pages/RunsPage'), 'RunsPage');
const CanvasPage = lazyPage(() => import('./pages/CanvasPage'), 'CanvasPage');
const FilesPage = lazyPage(() => import('./pages/FilesPage'), 'FilesPage');
const AgentDetailPage = lazyPage(() => import('./pages/AgentDetailPage'), 'AgentDetailPage');
const SkillMarketPage = lazyPage(() => import('./pages/SkillMarketPage'), 'SkillMarketPage');
const ExpertsPage = lazyPage(() => import('./pages/ExpertsPage'), 'ExpertsPage');
const GalleryPage = lazyPage(() => import('./pages/GalleryPage'), 'GalleryPage');
const MailPage = lazyPage(() => import('./pages/MailPage'), 'MailPage');
const WidgetsPage = lazyPage(() => import('./pages/WidgetsPage'), 'WidgetsPage');
const WidgetComposerPage = lazyPage(() => import('./pages/WidgetComposerPage'), 'WidgetComposerPage');
const IntegrationsPage = lazyPage(() => import('./pages/IntegrationsPage'), 'IntegrationsPage');
const BillingShell = lazyPage(() => import('./pages/BillingShell'), 'BillingShell');
const GovernanceShell = lazyPage(() => import('./pages/GovernanceShell'), 'GovernanceShell');
const LicenseShell = lazyPage(() => import('./pages/LicenseShell'), 'LicenseShell');
const WebChatPage = lazyPage(() => import('./pages/WebChatPage'), 'WebChatPage');
// O-2 (`DESIGN-agent-os-native-apps-2026-08.md` §6.3) — the conversational
// "operate the whole machine" scaffold; see `HomeLanding` below for how the
// appliance image lands here by default.
const OperatorConsolePage = lazyPage(() => import('./pages/OperatorConsolePage'), 'OperatorConsolePage');
const AgentsPage = lazyPage(() => import('./pages/AgentsPage'), 'AgentsPage');
const CreateAgentPage = lazyPage(() => import('./pages/agent-form/CreateAgentPage'), 'CreateAgentPage');
const EditAgentPage = lazyPage(() => import('./pages/agent-form/EditAgentPage'), 'EditAgentPage');
const TaskBoardPage = lazyPage(() => import('./pages/TaskBoardPage'), 'TaskBoardPage');
const PlansPage = lazyPage(() => import('./pages/PlansPage'), 'PlansPage');
const GoalsPage = lazyPage(() => import('./pages/GoalsPage'), 'GoalsPage');
const ForesightPage = lazyPage(() => import('./pages/ForesightPage'), 'ForesightPage');
const ForkPage = lazyPage(() => import('./pages/ForkPage'), 'ForkPage');
const MarketplacePage = lazyPage(() => import('./pages/MarketplacePage'), 'MarketplacePage');
const MemoryPage = lazyPage(() => import('./pages/MemoryPage'), 'MemoryPage');
const KnowledgeHubPage = lazyPage(() => import('./pages/KnowledgeHubPage'), 'KnowledgeHubPage');
const SharedWikiPage = lazyPage(() => import('./pages/SharedWikiPage'), 'SharedWikiPage');
const OrgChartPage = lazyPage(() => import('./pages/OrgChartPage'), 'OrgChartPage');
const WorldPage = lazyPage(() => import('./pages/WorldPage'), 'WorldPage');
// PartnerPortalPage is mounted inside LicenseShell (`/manage/license`); the
// bare-page import was only needed for the now-removed `/partner` legacy
// route below, which redirects instead.
const ReportPage = lazyPage(() => import('./pages/ReportPage'), 'ReportPage');
const BillingPage = lazyPage(() => import('./pages/BillingPage'), 'BillingPage');
const LogsPage = lazyPage(() => import('./pages/LogsPage'), 'LogsPage');
const ChannelsPage = lazyPage(() => import('./pages/ChannelsPage'), 'ChannelsPage');
const AccountsPage = lazyPage(() => import('./pages/AccountsPage'), 'AccountsPage');
const SecurityPage = lazyPage(() => import('./pages/SecurityPage'), 'SecurityPage');
const SecauditPage = lazyPage(() => import('./pages/SecauditPage'), 'SecauditPage');
const GovernancePage = lazyPage(() => import('./pages/GovernancePage'), 'GovernancePage');
const ReliabilityPage = lazyPage(() => import('./pages/ReliabilityPage'), 'ReliabilityPage');
const SettingsPage = lazyPage(() => import('./pages/SettingsPage'), 'SettingsPage');
const InferencePage = lazyPage(() => import('./pages/InferencePage'), 'InferencePage');
const LocalModelsPage = lazyPage(() => import('./pages/LocalModelsPage'), 'LocalModelsPage');
const UsersPage = lazyPage(() => import('./pages/UsersPage'), 'UsersPage');
const DepartmentsPage = lazyPage(() => import('./pages/DepartmentsPage'), 'DepartmentsPage');
const MigratePage = lazyPage(() => import('./pages/MigratePage'), 'MigratePage');
const WelcomePage = lazyPage(() => import('./pages/WelcomePage'), 'WelcomePage');
// N-1 (`DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-1) — the app-registry
// grid; the web-first half of the launcher the on-box shell (N-2) will reuse.
const LauncherPage = lazyPage(() => import('./pages/LauncherPage'), 'LauncherPage');
// N-3 (§5 WP N-3) — the 系統設定 app's own `/app/system` settings-hub home.
const SystemHomePage = lazyPage(() => import('./pages/SystemHomePage'), 'SystemHomePage');
// v2 redesign lazy placeholder pages (T1.5) — replaced in place by later waves.
const TaskDetailPage = lazyPage(() => import('./pages/TaskDetailPage'), 'TaskDetailPage');
const SkillNewPage = lazyPage(() => import('./pages/SkillNewPage'), 'SkillNewPage');
const SkillCustomDetailPage = lazyPage(() => import('./pages/SkillCustomDetailPage'), 'SkillCustomDetailPage');
const GrowthPage = lazyPage(() => import('./pages/GrowthPage'), 'GrowthPage');
const MascotOverlayPage = lazyPage(() => import('./pages/MascotOverlayPage'), 'MascotOverlayPage');
const GatewayPickerPage = lazyPage(() => import('./pages/GatewayPickerPage'), 'GatewayPickerPage');
const PetStudioPage = lazyPage(() => import('./pages/PetStudioPage'), 'PetStudioPage');
const AboutPage = lazyPage(() => import('./pages/AboutPage'), 'AboutPage');
const DistributorsPage = lazyPage(() => import('./pages/DistributorsPage'), 'DistributorsPage');
const OSPage = lazyPage(() => import('./pages/OSPage'), 'OSPage');
const PresetsPage = lazyPage(() => import('./pages/PresetsPage'), 'PresetsPage');
const DevicePage = lazyPage(() => import('./pages/DevicePage'), 'DevicePage');
// WP9 (2026-08-04 IA rework): full conversation history + the two management
// surfaces lifted out of billing / settings + the 進階設定 index.
const ConversationsPage = lazyPage(() => import('./pages/ConversationsPage'), 'ConversationsPage');
const SystemUpdatePage = lazyPage(() => import('./pages/SystemUpdatePage'), 'SystemUpdatePage');
const ManageAdvancedPage = lazyPage(() => import('./pages/ManageAdvancedPage'), 'ManageAdvancedPage');

/** Lightweight route-transition fallback while a lazy page chunk loads. */
function PageFallback() {
  return (
    <div className="flex h-full items-center justify-center py-20" role="status" aria-live="polite">
      <span className="h-6 w-6 animate-spin rounded-full border-2 border-brand/30 border-t-brand" />
    </div>
  );
}

/**
 * O-2 landing gate (`DESIGN-agent-os-native-apps-2026-08.md` §6.2 "對話先行":
 * appliance 的預設畫面是全螢幕對話式主控台，不是 app 網格). The bare `/` index
 * route renders the conversational console instead of the dashboard grid once
 * `system.status`'s `is_appliance` field confirms this gateway IS the
 * DuDuClaw appliance image; every other install (the overwhelming majority)
 * is unaffected and keeps `HomePage` exactly as before. `/workspace` —
 * HomePage's other alias — is left pointing straight at `HomePage` so "go to
 * the dashboard" always has a literal, ungated destination even on an
 * appliance (§6 O-5: nothing native is removed, only the default first door
 * changes).
 *
 * R2 (2026-08): every authenticated role gets this redirect, not just admin.
 * The original gate reused `useIsAppliance(hasMinRole(role, 'admin'))` — the
 * same pattern `AppSidebar`/`CommandPalette` use for their `/device` nav
 * probe — because that hook used to read the admin-only `device.status` RPC
 * (`require_admin!()` in `handlers.rs`), so a manager/employee viewer could
 * never learn whether the box was an appliance and silently stayed on
 * `HomePage`. `useIsAppliance` now reads the non-admin `is_appliance` field
 * on `system.status` instead, so this route no longer needs a role gate at
 * all — `HomeLanding` only renders once `AuthGuard` has already confirmed
 * the caller is signed in, and `/console` itself is open to every role
 * (see its route below). `AppSidebar`/`CommandPalette` keep their own
 * `hasMinRole(role, 'admin')` argument unchanged — that gate is about
 * showing the admin-only `/device` page, a separate concern from this one.
 */
function HomeLanding() {
  const isAppliance = useIsAppliance(true);
  if (isAppliance) return <Navigate to="/console" replace />;
  return <HomePage />;
}

export function App() {
  const connectWithAuth = useConnectionStore((s) => s.connectWithAuth);
  const disconnect = useConnectionStore((s) => s.disconnect);
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const initialized = useAuthStore((s) => s.initialized);
  const navigate = useNavigate();

  // The desktop pet's right-click "open studio" item fires a Tauri event on the
  // main window; navigate there when it arrives (no-op outside Tauri).
  useEffect(() => {
    let unlisten = () => {};
    let alive = true;
    void onPetOpenStudio(() => navigate('/pet-studio')).then((fn) => {
      if (alive) unlisten = fn;
      else fn();
    });
    return () => {
      alive = false;
      unlisten();
    };
  }, [navigate]);

  // B5: server-initiated dashboard navigation (`dashboard.navigate` WS event
  // — see `lib/dashboard-navigate.ts` for the cooldown + mid-edit-form guard).
  // Subscribed once, globally, so every page benefits without per-page wiring.
  useEffect(() => {
    return client.subscribe('dashboard.navigate', (payload) => {
      handleDashboardNavigate(payload, navigate, (message, action) =>
        toast.info(message, { action }),
      );
    });
  }, [navigate]);

  // Connect WS after auth is resolved; disconnect on logout.
  // Skip during initialization to avoid premature disconnect.
  useEffect(() => {
    if (!initialized) return;
    if (isAuthenticated) {
      connectWithAuth(() => useAuthStore.getState().jwt ?? undefined);
    } else {
      disconnect();
    }
  }, [initialized, isAuthenticated, connectWithAuth, disconnect]);

  return (
    <>
      <ApprovalModal />
      <Suspense fallback={<PageFallback />}>
        <Routes>
          <Route path="login" element={<LoginPage />} />
          {/* `/wizard` was a second, older first-run wizard that nothing linked
              to and `FirstRunGate` never sent anyone to (§2-13). Its component
              is gone; the path redirects so any stale bookmark lands on the one
              real onboarding surface. */}
          <Route path="wizard" element={<Navigate to="/welcome" replace />} />
          {/* Tauri desktop-pet mini route (§7.4) — no app shell. */}
          <Route path="mascot-overlay" element={<MascotOverlayPage />} />
          {/* WP-GW desktop Gateway picker — pre-login, no app shell; redirects
              to `/` outside Tauri. The desktop main window boots here. */}
          <Route path="gateway-picker" element={<GatewayPickerPage />} />
          <Route element={<AuthGuard />}>
            <Route element={<MainLayout />}>
              {/* First-run onboarding — mounted OUTSIDE FirstRunGate so the
                  zero-agent redirect target itself is never gated (no loop). */}
              <Route path="welcome" element={<WelcomePage />} />

              {/* N-1 deep-link seam (§2 L2 + §5 WP N-1/N-3): /app/<id>/<rest>
                  redirects to the existing canonical route at /<rest> (or the
                  app's default path with no rest). Mounted outside
                  FirstRunGate, like `welcome` above — the redirect TARGET is
                  what carries FirstRunGate (most of them do), so gating
                  cascades naturally on the next render instead of being
                  duplicated here. No pages have moved; see
                  `apps/AppRouteRedirect.tsx` for the full rationale. */}
              <Route path="app/:appId/*" element={<AppRouteRedirect />} />

              {/* Everything else requires at least one agent to exist. */}
              <Route element={<FirstRunGate />}>
              {/* ── Zone A 每日 — open to all authenticated users ──
                  Home is the single spine: a read-only overview (the former
                  workspace mode was collapsed into it, so `/workspace` renders
                  the same page). It carries no composer — the 交辦 panel is
                  global and mounted in `MainLayout` (UX plan I-1a); this
                  comment used to claim a "launcher hero" that no version of
                  HomePage ever rendered. */}
              {/* O-2: appliance lands on the conversational console; every
                  other install still lands on HomePage (see `HomeLanding`). */}
              <Route index element={<HomeLanding />} />
              <Route path="workspace" element={<HomePage />} />
              <Route path="inbox" element={<InboxPage />} />
              {/* v2 (T1.5): /webchat renamed to /chat; old path redirects. */}
              <Route path="chat" element={<WebChatPage />} />
              <Route path="webchat" element={<Navigate to="/chat" replace />} />
              {/* O-2 (§6.3) — the operator console scaffold. Open to every
                  authenticated role, like `/chat`: the capability gating that
                  matters lives at the tool layer (O-0), not the nav entry. */}
              <Route path="console" element={<OperatorConsolePage />} />
              {/* Full conversation history (2026-08-04, D17) — the sidebar zone
                  lists the newest five and links here for the rest. */}
              <Route path="conversations" element={<ConversationsPage />} />

              {/* ── 工作 ── */}
              <Route path="tasks" element={<TaskBoardPage />} />
              <Route path="tasks/:id" element={<TaskDetailPage />} />
              <Route path="goals" element={<GoalsPage />} />
              <Route path="foresight" element={<ForesightPage />} />
              {/* U4 co-edited plans — shared step lists between the user and
                  an AI employee (agent-scoped; the gateway fails closed). */}
              <Route path="plans" element={<PlansPage />} />
              {/* G12 run inspector — data-scoped (the gateway fails closed
                  per agent), so it is open to every authenticated user. */}
              <Route path="runs" element={<RunsPage />} />
              <Route path="canvas" element={<CanvasPage />} />
              <Route path="files" element={<FilesPage />} />
              {/* D7 (09-edition-split-features.md §4): dropped out of the
                  manager+ RoleGuard block below — a cron schedule for one's own
                  agent is the same "own scope" as /plans and /runs above, not a
                  manager-level concern. */}
              <Route path="routines" element={<RoutinesPage />} />

              {/* ── 員工 / 公司 ── */}
              <Route path="agents" element={<AgentsPage />} />
              {/* Create / edit forms are standalone pages (formerly dialogs on
                  the roster). Declared before agents/:id so the static segments
                  win over the dynamic ones. */}
              <Route path="agents/new" element={<CreateAgentPage />} />
              <Route path="agents/:id/edit" element={<EditAgentPage />} />
              {/* The immersive full-bleed world page (PixiJS 2D iso). The Home
                  band and Org "世界" tab both link here so the heavy scene mounts
                  in exactly one place. */}
              <Route path="world" element={<WorldPage />} />
              <Route path="agents/:id" element={<AgentDetailPage />} />
              <Route path="agents/:id/:tab" element={<AgentDetailPage />} />
              <Route path="memory" element={<MemoryPage />} />
              <Route path="growth" element={<GrowthPage />} />
              {/* 桌寵工作室 — photo → interactive desktop pet (WP-P2/P3). */}
              <Route path="pet-studio" element={<PetStudioPage />} />
              <Route path="skills" element={<SkillMarketPage />} />
              <Route path="skills/new" element={<SkillNewPage />} />
              <Route path="skills/custom/:id" element={<SkillCustomDetailPage />} />
              {/* Widget 工坊 — custom dashboard widgets (design 2026-07-16). */}
              <Route path="widgets" element={<WidgetsPage />} />
              <Route path="widgets/new" element={<WidgetComposerPage />} />
              <Route path="widgets/:id/edit" element={<WidgetComposerPage />} />
              {/* 2026-07-30: the knowledge base merged into 記憶. Old links
                  (bookmarks, the guided tour, docs) land on the knowledge tab
                  instead of a dead route. */}
              <Route path="knowledge" element={<Navigate to="/memory?tab=wiki" replace />} />
              {/* 關於 — open to every authenticated user (all instances). */}
              <Route path="about" element={<AboutPage />} />
              {/* N-1 launcher (§2 L3 + §5 WP N-1) — app grid, open to every
                  authenticated role (each card's own visibility still gates
                  per-app via isAppVisible). Not a nav item by design — see
                  `LauncherPage.tsx`'s doc comment. */}
              <Route path="launcher" element={<LauncherPage />} />

              {/* N-3 (§5 WP N-3): 系統設定 app canonical routes. These six
                  pages physically left `/manage/*` (and, for `device`, the
                  bare top-level `/device`) — this IS their home now, not a
                  redirect stand-in. Role gates are copied byte-for-byte from
                  where each page used to sit: `license` was the one
                  manager-level surface among them (`manageAdvancedNav`'s
                  `personalHidden` license row), the other five were
                  admin-level (the P4-3 `/device` block below used to gate
                  `device`; the former `/manage` admin block gated the rest).
                  The bare index is admin-gated too — it matches the app's own
                  `minRole: 'admin'` in `apps/registry.ts`, and a manager who
                  still needs `license` reaches it by its own direct route,
                  not through this grid. */}
              <Route path="app/system">
                <Route element={<RoleGuard minRole="admin" />}>
                  <Route index element={<SystemHomePage />} />
                  <Route path="device" element={<DevicePage />} />
                  <Route path="updates" element={<SystemUpdatePage />} />
                  <Route path="accounts" element={<AccountsPage />} />
                  <Route path="security" element={<SecurityPage />} />
                  <Route path="settings" element={<SettingsPage />} />
                </Route>
                <Route element={<RoleGuard minRole="manager" />}>
                  <Route path="license" element={<LicenseShell />} />
                </Route>
              </Route>

              {/* manager+ routes (Zone B/C) */}
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="forks" element={<ForkPage />} />
                <Route path="timeline" element={<TimelinePage />} />
                <Route path="reports" element={<ReportPage />} />
                <Route path="org" element={<OrgChartPage />} />
                {/* 信箱 (Agent Mail, P2-d) — every `mail.*` RPC is
                    `require_manager!`-gated server-side; this mirrors that. */}
                <Route path="mail" element={<MailPage />} />
              </Route>

              {/* P4-3 — OS-native fleet report + settings. All five os.* RPCs
                  are `require_admin!`-gated server-side; this mirrors that. */}
              <Route element={<RoleGuard minRole="admin" />}>
                <Route path="os" element={<OSPage />} />
                {/* 職務組合（agent preset P1, WP-7I）— presets.list/presets.status
                    are read-only RPCs; this mirrors that (no admin RPC gate on
                    presets.list itself, but preset content is capability/
                    security-relevant, matching its `/os` neighbour's gate). */}
                <Route path="presets" element={<PresetsPage />} />
                {/* 裝置（WP-C）relocated to `/app/system/device` (N-3). Old
                    bookmarks/links redirect there — same admin gate, query
                    string preserved. */}
                <Route path="device" element={<LegacyRouteRedirect to="/app/system/device" />} />
                {/* 專家包 — expert-pack management; experts.* RPCs are
                    require_admin!-gated server-side, this mirrors that. */}
                <Route path="experts" element={<ExpertsPage />} />
                {/* 靈感畫廊 (P2-b) — curated "做同款" showcase; gallery.list
                    reads the same license-gated premium tree as
                    experts.catalog, require_admin!-gated the same way. */}
                <Route path="gallery" element={<GalleryPage />} />
              </Route>

              {/* ── Zone D 管理 — single entry, ManageShell subnav tree ──
                  ManageShell itself fail-closes to manager+; each child re-gates. */}
              <Route path="manage" element={<ManageShell />}>
                <Route element={<RoleGuard minRole="manager" />}>
                  <Route path="billing" element={<BillingShell />} />
                  {/* 授權 relocated to `/app/system/license` (N-3). */}
                  <Route path="license" element={<LegacyRouteRedirect to="/app/system/license" />} />
                  <Route path="migrate" element={<MigratePage />} />
                  <Route path="logs" element={<LogsPage />} />
                  {/* 安全審計（secaudit dashboard）— manager+, not admin-only:
                      reviewing a finding is closer to analytics.* than the
                      admin-gated credential-rotation surfaces below. */}
                  <Route path="secaudit" element={<SecauditPage />} />
                  {/* 進階設定 index — the surfaces folded a level down (D18). */}
                  <Route path="advanced" element={<ManageAdvancedPage />} />
                </Route>
                <Route element={<RoleGuard minRole="admin" />}>
                  <Route path="channels" element={<ChannelsPage />} />
                  {/* 帳戶與登入 / 系統更新 relocated to `/app/system/*` (N-3) —
                      see the `system` app route block above `manage` used to
                      be promoted out of billing / settings into here (D18);
                      N-3 promotes them one level further, out of 管理 entirely. */}
                  <Route path="accounts" element={<LegacyRouteRedirect to="/app/system/accounts" />} />
                  <Route path="updates" element={<LegacyRouteRedirect to="/app/system/updates" />} />
                  <Route path="integrations" element={<IntegrationsPage />} />
                  <Route path="inference" element={<InferencePage />} />
                  <Route path="local-models" element={<LocalModelsPage />} />
                  <Route path="reliability" element={<ReliabilityPage />} />
                  {/* 安全 / 設定 relocated to `/app/system/*` (N-3). */}
                  <Route path="security" element={<LegacyRouteRedirect to="/app/system/security" />} />
                  <Route path="system" element={<LegacyRouteRedirect to="/app/system/settings" />} />
                  {/* Enterprise-only surfaces (D8/D10-B). The rail already hides
                      these on a Personal instance; `EditionGuard` closes the URL
                      too, so a bookmark or a typed path can't reach a console
                      whose feature that edition doesn't have. `personalHidden`
                      surfaces (可靠性 / 授權 — 安全 / 日誌 dropped this gate
                      under D9) stay reachable on purpose — see the guard's own
                      note. */}
                  <Route element={<EditionGuard enterprise />}>
                    <Route path="governance" element={<GovernanceShell />} />
                    <Route path="users" element={<UsersPage />} />
                    <Route path="departments" element={<DepartmentsPage />} />
                    <Route path="distributors" element={<DistributorsPage />} />
                  </Route>
                </Route>
              </Route>

              {/* ── Legacy route aliases (bookmarks keep working; §0 可回滾) ── */}
              {/* The old overview page was an earlier draft of `/` and nothing
                  linked to it (§2-13); its component is gone. */}
              <Route path="legacy-dashboard" element={<Navigate to="/" replace />} />
              <Route path="marketplace" element={<MarketplacePage />} />
              <Route path="wiki" element={<KnowledgeHubPage />} />
              <Route path="shared-wiki" element={<SharedWikiPage />} />
              {/* The approval centre showed the same decisions as the inbox but
                  with less of the information needed to make them safely — no
                  risk badge, no second confirmation on a high-risk action, and a
                  new-skill request reduced to a plain approve/reject with the
                  skill's own content and security scan nowhere in sight (§2-6).
                  One door for decisions; this one redirects to it. */}
              <Route path="approvals" element={<Navigate to="/inbox" replace />} />
              {/* These three are the source files behind 管理 → 整合's tabs, not
                  standalone pages (§2-8). Their old paths land on the tab that
                  actually renders them — MCP access keys share the 工具伺服器 tab. */}
              <Route path="mcp" element={<Navigate to="/manage/integrations?tab=mcp" replace />} />
              <Route path="mcp-keys" element={<Navigate to="/manage/integrations?tab=mcp" replace />} />
              <Route path="odoo" element={<Navigate to="/manage/integrations?tab=odoo" replace />} />
              {/* Orphan-page fixups (X04, audit phase4): these three used to render
                  a full standalone page with no nav-model / command-palette entry
                  pointing at it. Their content lives inside a `/manage/*` shell
                  now, so the old path redirects there instead of rendering a
                  second, unreachable-except-by-URL copy. */}
              {/* N-3: both hops now land straight on the current canonical
                  `/app/system/license` instead of bouncing through the
                  now-also-redirecting `/manage/license`. */}
              <Route path="partner" element={<Navigate to="/app/system/license" replace />} />
              <Route path="wiki-trust" element={<Navigate to="/manage/governance?tab=wikiTrust" replace />} />
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="billing" element={<BillingPage />} />
                {/* 授權 relocated to `/app/system/license` (N-3) — this legacy
                    alias used to render the bare `LicensePage` (pre-dating the
                    license+partner tab merge); it now lands on the same
                    canonical `LicenseShell` every other license route does. */}
                <Route path="license" element={<LegacyRouteRedirect to="/app/system/license" />} />
                <Route path="logs" element={<LogsPage />} />
              </Route>
              <Route element={<RoleGuard minRole="admin" />}>
                <Route path="channels" element={<Navigate to="/manage/channels" replace />} />
                {/* 帳戶與登入 / 安全 / 設定 relocated to `/app/system/*` (N-3). */}
                <Route path="accounts" element={<LegacyRouteRedirect to="/app/system/accounts" />} />
                <Route path="security" element={<LegacyRouteRedirect to="/app/system/security" />} />
                <Route path="reliability" element={<ReliabilityPage />} />
                <Route path="settings" element={<LegacyRouteRedirect to="/app/system/settings" />} />
                <Route path="inference" element={<InferencePage />} />
                <Route path="local-models" element={<LocalModelsPage />} />
                {/* D10-B: the aliases were the hole — every Enterprise-only page
                    had an ungated second path in here. Same guard as the
                    canonical routes above, so neither can be used to walk around
                    the other. */}
                <Route element={<EditionGuard enterprise />}>
                  <Route path="governance" element={<GovernancePage />} />
                  <Route path="users" element={<UsersPage />} />
                </Route>
              </Route>
              </Route>{/* end FirstRunGate */}
            </Route>
          </Route>
        </Routes>
      </Suspense>
    </>
  );
}
