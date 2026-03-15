import { useEffect } from 'react';
import { Routes, Route } from 'react-router';
import { MainLayout } from './components/layout/MainLayout';
import { DashboardPage } from './pages/DashboardPage';
import { AgentsPage } from './pages/AgentsPage';
import { ChannelsPage } from './pages/ChannelsPage';
import { AccountsPage } from './pages/AccountsPage';
import { MemoryPage } from './pages/MemoryPage';
import { SecurityPage } from './pages/SecurityPage';
import { SettingsPage } from './pages/SettingsPage';
import { LogsPage } from './pages/LogsPage';
import { useConnectionStore } from './stores/connection-store';

export function App() {
  const connect = useConnectionStore((s) => s.connect);

  useEffect(() => {
    connect();
  }, [connect]);

  return (
    <Routes>
      <Route element={<MainLayout />}>
        <Route index element={<DashboardPage />} />
        <Route path="agents" element={<AgentsPage />} />
        <Route path="channels" element={<ChannelsPage />} />
        <Route path="accounts" element={<AccountsPage />} />
        <Route path="memory" element={<MemoryPage />} />
        <Route path="security" element={<SecurityPage />} />
        <Route path="settings" element={<SettingsPage />} />
        <Route path="logs" element={<LogsPage />} />
      </Route>
    </Routes>
  );
}
