import { useEffect } from 'react';
import { Routes, Route } from 'react-router';
import { MainLayout } from './components/layout/MainLayout';
import { AuthGuard, RoleGuard } from './components/AuthGuard';
import { LoginPage } from './pages/LoginPage';
import { DashboardPage } from './pages/DashboardPage';
import { AgentsPage } from './pages/AgentsPage';
import { ChannelsPage } from './pages/ChannelsPage';
import { AccountsPage } from './pages/AccountsPage';
import { MemoryPage } from './pages/MemoryPage';
import { SecurityPage } from './pages/SecurityPage';
import { SettingsPage } from './pages/SettingsPage';
import { LogsPage } from './pages/LogsPage';
import { OrgChartPage } from './pages/OrgChartPage';
import { SkillMarketPage } from './pages/SkillMarketPage';
import { WebChatPage } from './pages/WebChatPage';
import { BillingPage } from './pages/BillingPage';
import { ReportPage } from './pages/ReportPage';
import { KnowledgeHubPage } from './pages/KnowledgeHubPage';
import { SharedWikiPage } from './pages/SharedWikiPage';
import { OnboardWizardPage } from './pages/OnboardWizardPage';
import { OdooPage } from './pages/OdooPage';
import { UsersPage } from './pages/UsersPage';
import { McpPage } from './pages/McpPage';
import { TaskBoardPage } from './pages/TaskBoardPage';
import { MarketplacePage } from './pages/MarketplacePage';
import { PartnerPortalPage } from './pages/PartnerPortalPage';
import { ReliabilityPage } from './pages/ReliabilityPage';
import { WikiTrustPage } from './pages/WikiTrustPage';
import { useConnectionStore } from './stores/connection-store';
import { useAuthStore } from './stores/auth-store';
import { ApprovalModal } from './components/ApprovalModal';

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
    <Routes>
      <Route path="login" element={<LoginPage />} />
      <Route path="wizard" element={<OnboardWizardPage />} />
      <Route element={<AuthGuard />}>
        <Route element={<MainLayout />}>
          {/* Open to all authenticated users */}
          <Route index element={<DashboardPage />} />
          <Route path="webchat" element={<WebChatPage />} />
          <Route path="agents" element={<AgentsPage />} />
          <Route path="tasks" element={<TaskBoardPage />} />
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
            <Route path="logs" element={<LogsPage />} />
          </Route>

          {/* admin-only routes */}
          <Route element={<RoleGuard minRole="admin" />}>
            <Route path="channels" element={<ChannelsPage />} />
            <Route path="accounts" element={<AccountsPage />} />
            <Route path="security" element={<SecurityPage />} />
            <Route path="reliability" element={<ReliabilityPage />} />
            <Route path="wiki-trust" element={<WikiTrustPage />} />
            <Route path="settings" element={<SettingsPage />} />
            <Route path="mcp" element={<McpPage />} />
            <Route path="odoo" element={<OdooPage />} />
            <Route path="users" element={<UsersPage />} />
          </Route>
        </Route>
      </Route>
    </Routes>
    </>
  );
}
