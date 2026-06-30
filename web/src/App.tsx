import { useEffect, lazy, Suspense } from 'react';
import { Routes, Route } from 'react-router';
import { MainLayout } from './components/layout/MainLayout';
import { AuthGuard, RoleGuard } from './components/AuthGuard';
import { FirstRunGate } from './components/FirstRunGate';
import { LoginPage } from './pages/LoginPage';
import { useConnectionStore } from './stores/connection-store';
import { useAuthStore } from './stores/auth-store';
import { useUiModeStore } from './stores/ui-mode-store';
import { ApprovalModal } from './components/ApprovalModal';

// Code-splitting: every authenticated page is lazy-loaded so heavy, page-only
// dependencies (d3 for WikiGraph/OrgChart, large forms) land in their own route
// chunk instead of the main bundle. LoginPage stays eager for instant first paint.
// Named exports are adapted to lazy()'s default-export contract inline.
const lazyPage = <K extends string>(loader: () => Promise<Record<K, React.ComponentType>>, key: K) =>
  lazy(() => loader().then((m) => ({ default: m[key] })));

const DashboardPage = lazyPage(() => import('./pages/DashboardPage'), 'DashboardPage');
const WorkspacePage = lazyPage(() => import('./pages/WorkspacePage'), 'WorkspacePage');
const WebChatPage = lazyPage(() => import('./pages/WebChatPage'), 'WebChatPage');
const AgentsPage = lazyPage(() => import('./pages/AgentsPage'), 'AgentsPage');
const TaskBoardPage = lazyPage(() => import('./pages/TaskBoardPage'), 'TaskBoardPage');
const ForkPage = lazyPage(() => import('./pages/ForkPage'), 'ForkPage');
const SkillMarketPage = lazyPage(() => import('./pages/SkillMarketPage'), 'SkillMarketPage');
const MarketplacePage = lazyPage(() => import('./pages/MarketplacePage'), 'MarketplacePage');
const MemoryPage = lazyPage(() => import('./pages/MemoryPage'), 'MemoryPage');
const KnowledgeHubPage = lazyPage(() => import('./pages/KnowledgeHubPage'), 'KnowledgeHubPage');
const SharedWikiPage = lazyPage(() => import('./pages/SharedWikiPage'), 'SharedWikiPage');
const OrgChartPage = lazyPage(() => import('./pages/OrgChartPage'), 'OrgChartPage');
const PartnerPortalPage = lazyPage(() => import('./pages/PartnerPortalPage'), 'PartnerPortalPage');
const ReportPage = lazyPage(() => import('./pages/ReportPage'), 'ReportPage');
const BillingPage = lazyPage(() => import('./pages/BillingPage'), 'BillingPage');
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
const OnboardWizardPage = lazyPage(() => import('./pages/OnboardWizardPage'), 'OnboardWizardPage');
const WelcomePage = lazyPage(() => import('./pages/WelcomePage'), 'WelcomePage');

/** Lightweight route-transition fallback while a lazy page chunk loads. */
function PageFallback() {
  return (
    <div className="flex h-full items-center justify-center py-20" role="status" aria-live="polite">
      <span className="h-6 w-6 animate-spin rounded-full border-2 border-amber-500/30 border-t-amber-500" />
    </div>
  );
}

/**
 * The index route renders the workspace or the dashboard depending on the
 * active shell mode (TODO-genspark-workspace-shell §P0.3). Rendering in place
 * (rather than redirecting) avoids a flash and keeps the URL at `/`.
 */
function HomeRoute() {
  const mode = useUiModeStore((s) => s.mode);
  return mode === 'workspace' ? <WorkspacePage /> : <DashboardPage />;
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
          <Route element={<AuthGuard />}>
            <Route element={<MainLayout />}>
              {/* First-run onboarding — mounted OUTSIDE FirstRunGate so the
                  zero-agent redirect target itself is never gated (no loop). */}
              <Route path="welcome" element={<WelcomePage />} />

              {/* Everything else requires at least one agent to exist. */}
              <Route element={<FirstRunGate />}>
              {/* Open to all authenticated users */}
              <Route index element={<HomeRoute />} />
              <Route path="workspace" element={<WorkspacePage />} />
              <Route path="webchat" element={<WebChatPage />} />
              <Route path="agents" element={<AgentsPage />} />
              <Route path="tasks" element={<TaskBoardPage />} />
              <Route path="forks" element={<ForkPage />} />
              <Route path="skills" element={<SkillMarketPage />} />
              <Route path="marketplace" element={<MarketplacePage />} />
              <Route path="memory" element={<MemoryPage />} />
              <Route path="wiki" element={<KnowledgeHubPage />} />
              <Route path="shared-wiki" element={<SharedWikiPage />} />

              {/* manager+ routes */}
              <Route element={<RoleGuard minRole="manager" />}>
                <Route path="org" element={<OrgChartPage />} />
                <Route path="partner" element={<PartnerPortalPage />} />
                <Route path="reports" element={<ReportPage />} />
                <Route path="billing" element={<BillingPage />} />
                <Route path="license" element={<LicensePage />} />
                <Route path="logs" element={<LogsPage />} />
              </Route>

              {/* admin-only routes */}
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
