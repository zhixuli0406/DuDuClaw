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
const LicensePage = lazyPage(() => import('./pages/LicensePage'), 'LicensePage');
const LogsPage = lazyPage(() => import('./pages/LogsPage'), 'LogsPage');
const ChannelsPage = lazyPage(() => import('./pages/ChannelsPage'), 'ChannelsPage');
const AccountsPage = lazyPage(() => import('./pages/AccountsPage'), 'AccountsPage');
const SecurityPage = lazyPage(() => import('./pages/SecurityPage'), 'SecurityPage');
const GovernancePage = lazyPage(() => import('./pages/GovernancePage'), 'GovernancePage');
const ReliabilityPage = lazyPage(() => import('./pages/ReliabilityPage'), 'ReliabilityPage');
const SettingsPage = lazyPage(() => import('./pages/SettingsPage'), 'SettingsPage');
const InferencePage = lazyPage(() => import('./pages/InferencePage'), 'InferencePage');
const LocalModelsPage = lazyPage(() => import('./pages/LocalModelsPage'), 'LocalModelsPage');
const UsersPage = lazyPage(() => import('./pages/UsersPage'), 'UsersPage');
const DepartmentsPage = lazyPage(() => import('./pages/DepartmentsPage'), 'DepartmentsPage');
const MigratePage = lazyPage(() => import('./pages/MigratePage'), 'MigratePage');
const WelcomePage = lazyPage(() => import('./pages/WelcomePage'), 'WelcomePage');
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

              {/* Everything else requires at least one agent to exist. */}
              <Route element={<FirstRunGate />}>
              {/* ── Zone A 每日 — open to all authenticated users ──
                  Home is the single spine: a read-only overview (the former
                  workspace mode was collapsed into it, so `/workspace` renders
                  the same page). It carries no composer — the 交辦 panel is
                  global and mounted in `MainLayout` (UX plan I-1a); this
                  comment used to claim a "launcher hero" that no version of
                  HomePage ever rendered. */}
              <Route index element={<HomePage />} />
              <Route path="workspace" element={<HomePage />} />
              <Route path="inbox" element={<InboxPage />} />
              {/* v2 (T1.5): /webchat renamed to /chat; old path redirects. */}
              <Route path="chat" element={<WebChatPage />} />
              <Route path="webchat" element={<Navigate to="/chat" replace />} />
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
                  <Route path="license" element={<LicenseShell />} />
                  <Route path="migrate" element={<MigratePage />} />
                  <Route path="logs" element={<LogsPage />} />
                  {/* 進階設定 index — the surfaces folded a level down (D18). */}
                  <Route path="advanced" element={<ManageAdvancedPage />} />
                </Route>
                <Route element={<RoleGuard minRole="admin" />}>
                  <Route path="channels" element={<ChannelsPage />} />
                  {/* 帳戶與登入 / 系統更新 — promoted out of billing / settings. */}
                  <Route path="accounts" element={<AccountsPage />} />
                  <Route path="updates" element={<SystemUpdatePage />} />
                  <Route path="integrations" element={<IntegrationsPage />} />
                  <Route path="inference" element={<InferencePage />} />
                  <Route path="local-models" element={<LocalModelsPage />} />
                  <Route path="reliability" element={<ReliabilityPage />} />
                  <Route path="security" element={<SecurityPage />} />
                  <Route path="system" element={<SettingsPage />} />
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
              <Route path="partner" element={<Navigate to="/manage/license" replace />} />
              <Route path="wiki-trust" element={<Navigate to="/manage/governance?tab=wikiTrust" replace />} />
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="billing" element={<BillingPage />} />
                <Route path="license" element={<LicensePage />} />
                <Route path="logs" element={<LogsPage />} />
              </Route>
              <Route element={<RoleGuard minRole="admin" />}>
                <Route path="channels" element={<Navigate to="/manage/channels" replace />} />
                <Route path="accounts" element={<AccountsPage />} />
                <Route path="security" element={<SecurityPage />} />
                <Route path="reliability" element={<ReliabilityPage />} />
                <Route path="settings" element={<SettingsPage />} />
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
