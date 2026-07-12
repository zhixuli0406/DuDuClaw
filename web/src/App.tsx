import { useEffect, lazy, Suspense } from 'react';
import { Routes, Route, Navigate } from 'react-router';
import { MainLayout } from './components/layout/MainLayout';
import { ManageShell } from './components/layout/ManageShell';
import { AuthGuard, RoleGuard } from './components/AuthGuard';
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

const DashboardPage = lazyPage(() => import('./pages/DashboardPage'), 'DashboardPage');
const HomePage = lazyPage(() => import('./pages/HomePage'), 'HomePage');
const InboxPage = lazyPage(() => import('./pages/InboxPage'), 'InboxPage');
const RoutinesPage = lazyPage(() => import('./pages/RoutinesPage'), 'RoutinesPage');
const TimelinePage = lazyPage(() => import('./pages/TimelinePage'), 'TimelinePage');
const RunsPage = lazyPage(() => import('./pages/RunsPage'), 'RunsPage');
const CanvasPage = lazyPage(() => import('./pages/CanvasPage'), 'CanvasPage');
const AgentDetailPage = lazyPage(() => import('./pages/AgentDetailPage'), 'AgentDetailPage');
const SkillsShell = lazyPage(() => import('./pages/SkillsShell'), 'SkillsShell');
const KnowledgeShell = lazyPage(() => import('./pages/KnowledgeShell'), 'KnowledgeShell');
const IntegrationsPage = lazyPage(() => import('./pages/IntegrationsPage'), 'IntegrationsPage');
const BillingShell = lazyPage(() => import('./pages/BillingShell'), 'BillingShell');
const GovernanceShell = lazyPage(() => import('./pages/GovernanceShell'), 'GovernanceShell');
const LicenseShell = lazyPage(() => import('./pages/LicenseShell'), 'LicenseShell');
const WebChatPage = lazyPage(() => import('./pages/WebChatPage'), 'WebChatPage');
const AgentsPage = lazyPage(() => import('./pages/AgentsPage'), 'AgentsPage');
const TaskBoardPage = lazyPage(() => import('./pages/TaskBoardPage'), 'TaskBoardPage');
const PlansPage = lazyPage(() => import('./pages/PlansPage'), 'PlansPage');
const ForkPage = lazyPage(() => import('./pages/ForkPage'), 'ForkPage');
const MarketplacePage = lazyPage(() => import('./pages/MarketplacePage'), 'MarketplacePage');
const MemoryPage = lazyPage(() => import('./pages/MemoryPage'), 'MemoryPage');
const KnowledgeHubPage = lazyPage(() => import('./pages/KnowledgeHubPage'), 'KnowledgeHubPage');
const SharedWikiPage = lazyPage(() => import('./pages/SharedWikiPage'), 'SharedWikiPage');
const OrgChartPage = lazyPage(() => import('./pages/OrgChartPage'), 'OrgChartPage');
const WorldPage = lazyPage(() => import('./pages/WorldPage'), 'WorldPage');
const PartnerPortalPage = lazyPage(() => import('./pages/PartnerPortalPage'), 'PartnerPortalPage');
const ReportPage = lazyPage(() => import('./pages/ReportPage'), 'ReportPage');
const BillingPage = lazyPage(() => import('./pages/BillingPage'), 'BillingPage');
const ApprovalsPage = lazyPage(() => import('./pages/ApprovalsPage'), 'ApprovalsPage');
const LicensePage = lazyPage(() => import('./pages/LicensePage'), 'LicensePage');
const LogsPage = lazyPage(() => import('./pages/LogsPage'), 'LogsPage');
const ChannelsPage = lazyPage(() => import('./pages/ChannelsPage'), 'ChannelsPage');
const AccountsPage = lazyPage(() => import('./pages/AccountsPage'), 'AccountsPage');
const SecurityPage = lazyPage(() => import('./pages/SecurityPage'), 'SecurityPage');
const GovernancePage = lazyPage(() => import('./pages/GovernancePage'), 'GovernancePage');
const ReliabilityPage = lazyPage(() => import('./pages/ReliabilityPage'), 'ReliabilityPage');
const WikiTrustPage = lazyPage(() => import('./pages/WikiTrustPage'), 'WikiTrustPage');
const SettingsPage = lazyPage(() => import('./pages/SettingsPage'), 'SettingsPage');
const McpPage = lazyPage(() => import('./pages/McpPage'), 'McpPage');
const McpKeysPage = lazyPage(() => import('./pages/McpKeysPage'), 'McpKeysPage');
const OdooPage = lazyPage(() => import('./pages/OdooPage'), 'OdooPage');
const InferencePage = lazyPage(() => import('./pages/InferencePage'), 'InferencePage');
const UsersPage = lazyPage(() => import('./pages/UsersPage'), 'UsersPage');
const MigratePage = lazyPage(() => import('./pages/MigratePage'), 'MigratePage');
const OnboardWizardPage = lazyPage(() => import('./pages/OnboardWizardPage'), 'OnboardWizardPage');
const WelcomePage = lazyPage(() => import('./pages/WelcomePage'), 'WelcomePage');
// v2 redesign lazy placeholder pages (T1.5) — replaced in place by later waves.
const TaskDetailPage = lazyPage(() => import('./pages/TaskDetailPage'), 'TaskDetailPage');
const SkillNewPage = lazyPage(() => import('./pages/SkillNewPage'), 'SkillNewPage');
const SkillCustomDetailPage = lazyPage(() => import('./pages/SkillCustomDetailPage'), 'SkillCustomDetailPage');
const GrowthPage = lazyPage(() => import('./pages/GrowthPage'), 'GrowthPage');
const MascotOverlayPage = lazyPage(() => import('./pages/MascotOverlayPage'), 'MascotOverlayPage');
const AboutPage = lazyPage(() => import('./pages/AboutPage'), 'AboutPage');
const DistributorsPage = lazyPage(() => import('./pages/DistributorsPage'), 'DistributorsPage');

/** Lightweight route-transition fallback while a lazy page chunk loads. */
function PageFallback() {
  return (
    <div className="flex h-full items-center justify-center py-20" role="status" aria-live="polite">
      <span className="h-6 w-6 animate-spin rounded-full border-2 border-amber-500/30 border-t-amber-500" />
    </div>
  );
}

export function App() {
  const connectWithAuth = useConnectionStore((s) => s.connectWithAuth);
  const disconnect = useConnectionStore((s) => s.disconnect);
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const initialized = useAuthStore((s) => s.initialized);

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
          <Route path="wizard" element={<OnboardWizardPage />} />
          {/* Tauri desktop-pet mini route (§7.4) — no app shell. */}
          <Route path="mascot-overlay" element={<MascotOverlayPage />} />
          <Route element={<AuthGuard />}>
            <Route element={<MainLayout />}>
              {/* First-run onboarding — mounted OUTSIDE FirstRunGate so the
                  zero-agent redirect target itself is never gated (no loop). */}
              <Route path="welcome" element={<WelcomePage />} />

              {/* Everything else requires at least one agent to exist. */}
              <Route element={<FirstRunGate />}>
              {/* ── Zone A 每日 — open to all authenticated users ──
                  Home is the single spine: it carries the one-line launcher
                  hero at the top (former workspace mode was collapsed here). */}
              <Route index element={<HomePage />} />
              <Route path="workspace" element={<HomePage />} />
              <Route path="inbox" element={<InboxPage />} />
              {/* v2 (T1.5): /webchat renamed to /chat; old path redirects. */}
              <Route path="chat" element={<WebChatPage />} />
              <Route path="webchat" element={<Navigate to="/chat" replace />} />

              {/* ── 工作 ── */}
              <Route path="tasks" element={<TaskBoardPage />} />
              <Route path="tasks/:id" element={<TaskDetailPage />} />
              {/* U4 co-edited plans — shared step lists between the user and
                  an AI employee (agent-scoped; the gateway fails closed). */}
              <Route path="plans" element={<PlansPage />} />
              {/* G12 run inspector — data-scoped (the gateway fails closed
                  per agent), so it is open to every authenticated user. */}
              <Route path="runs" element={<RunsPage />} />
              <Route path="canvas" element={<CanvasPage />} />

              {/* ── 員工 / 公司 ── */}
              <Route path="agents" element={<AgentsPage />} />
              {/* The immersive full-bleed world page (PixiJS 2D iso). The Home
                  band and Org "世界" tab both link here so the heavy scene mounts
                  in exactly one place. */}
              <Route path="world" element={<WorldPage />} />
              <Route path="agents/:id" element={<AgentDetailPage />} />
              <Route path="agents/:id/:tab" element={<AgentDetailPage />} />
              <Route path="memory" element={<MemoryPage />} />
              <Route path="growth" element={<GrowthPage />} />
              <Route path="skills" element={<SkillsShell />} />
              <Route path="skills/new" element={<SkillNewPage />} />
              <Route path="skills/custom/:id" element={<SkillCustomDetailPage />} />
              <Route path="knowledge" element={<KnowledgeShell />} />
              {/* 關於 — open to every authenticated user (all instances). */}
              <Route path="about" element={<AboutPage />} />

              {/* manager+ routes (Zone B/C) */}
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="forks" element={<ForkPage />} />
                <Route path="routines" element={<RoutinesPage />} />
                <Route path="timeline" element={<TimelinePage />} />
                <Route path="reports" element={<ReportPage />} />
                <Route path="org" element={<OrgChartPage />} />
              </Route>

              {/* ── Zone D 管理 — single entry, ManageShell subnav tree ──
                  ManageShell itself fail-closes to manager+; each child re-gates. */}
              <Route path="manage" element={<ManageShell />}>
                <Route element={<RoleGuard minRole="manager" />}>
                  <Route path="billing" element={<BillingShell />} />
                  <Route path="license" element={<LicenseShell />} />
                  <Route path="migrate" element={<MigratePage />} />
                  <Route path="logs" element={<LogsPage />} />
                </Route>
                <Route element={<RoleGuard minRole="admin" />}>
                  <Route path="channels" element={<ChannelsPage />} />
                  <Route path="integrations" element={<IntegrationsPage />} />
                  <Route path="inference" element={<InferencePage />} />
                  <Route path="reliability" element={<ReliabilityPage />} />
                  <Route path="security" element={<SecurityPage />} />
                  <Route path="governance" element={<GovernanceShell />} />
                  <Route path="users" element={<UsersPage />} />
                  <Route path="distributors" element={<DistributorsPage />} />
                  <Route path="system" element={<SettingsPage />} />
                </Route>
              </Route>

              {/* ── Legacy route aliases (bookmarks keep working; §0 可回滾) ── */}
              <Route path="legacy-dashboard" element={<DashboardPage />} />
              <Route path="marketplace" element={<MarketplacePage />} />
              <Route path="wiki" element={<KnowledgeHubPage />} />
              <Route path="shared-wiki" element={<SharedWikiPage />} />
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="approvals" element={<ApprovalsPage />} />
                <Route path="partner" element={<PartnerPortalPage />} />
                <Route path="billing" element={<BillingPage />} />
                <Route path="license" element={<LicensePage />} />
                <Route path="logs" element={<LogsPage />} />
              </Route>
              <Route element={<RoleGuard minRole="admin" />}>
                <Route path="channels" element={<ChannelsPage />} />
                <Route path="accounts" element={<AccountsPage />} />
                <Route path="security" element={<SecurityPage />} />
                <Route path="governance" element={<GovernancePage />} />
                <Route path="reliability" element={<ReliabilityPage />} />
                <Route path="wiki-trust" element={<WikiTrustPage />} />
                <Route path="settings" element={<SettingsPage />} />
                <Route path="mcp" element={<McpPage />} />
                <Route path="mcp-keys" element={<McpKeysPage />} />
                <Route path="odoo" element={<OdooPage />} />
                <Route path="inference" element={<InferencePage />} />
                <Route path="users" element={<UsersPage />} />
              </Route>
              </Route>{/* end FirstRunGate */}
            </Route>
          </Route>
        </Routes>
      </Suspense>
    </>
  );
}
