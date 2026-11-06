import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { useChatStore } from '@/stores/chat-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useCallableAgentsStore } from '@/stores/callable-agents-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { SidebarProvider } from '@/components/mds';
import { AppSidebar } from './layout/AppSidebar';
import { ConversationsPage } from '@/pages/ConversationsPage';
import { CommandPalette } from './CommandPalette';
import { WebChatPage } from '@/pages/WebChatPage';
import { AgentModelPicker } from './workspace/AgentModelPicker';
import { DataExportButton } from './DataExportButton';
import { MemoryPage } from '@/pages/MemoryPage';
import { KnowledgeHubPage } from '@/pages/KnowledgeHubPage';
import { AgentsPage } from '@/pages/AgentsPage';
import { FilesPage } from '@/pages/FilesPage';

import { ForesightPage } from '@/pages/ForesightPage';
import { GoalsPage } from '@/pages/GoalsPage';
import { CanvasPage } from '@/pages/CanvasPage';
import { RunsPage } from '@/pages/RunsPage';
import { TimelinePage } from '@/pages/TimelinePage';
import { PlansPage } from '@/pages/PlansPage';
import { RoutinesPage } from '@/pages/RoutinesPage';
import { TaskBoardPage } from '@/pages/TaskBoardPage';
import { MySkillsTab } from './skills/MySkillsTab';
import { SkillWizard } from './skills/SkillWizard';

const roster = ['AllowedStaff', 'OtherStaff', 'MainStaff', 'PublicStaff'].map(name => ({ name, display_name: name, role: name === 'MainStaff' ? 'main' : 'worker', status: 'active', icon: '', trigger: '', reports_to: '', model: { preferred: '' }, budget: { monthly_limit_cents: 0, spent_cents: 0 } }));
const callable = roster.filter(a => a.name !== 'OtherStaff').map(a => ({ ...a, via: a.name === 'PublicStaff' ? 'public' : 'binding', selectable: a.name === 'AllowedStaff', assignable: true, call_text: `@${a.name}` }));
let failed = false;
beforeEach(() => {
  vi.restoreAllMocks();
  failed = false;
  useAuthStore.setState({ user: { id: 'viewer', role: 'employee', display_name: 'Viewer' } as never, bindings: roster.map(a => ({ agent_name: a.name, access_level: 'operator' })) as never });
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
  useAgentsStore.setState({ agents: roster as never, loaded: true, loading: false, fetchAgents: vi.fn().mockResolvedValue(true) });
  useCallableAgentsStore.setState({ entries: {} });
  useConnectionStore.setState({ state: 'disconnected' });
  useChatStore.setState({ connectionState: 'connected', selectedAgentId: null, sessionId: null, messages: [], resolvedAgent: { id: 'AllowedStaff', name: 'AllowedStaff', icon: '' }, assignmentError: null, resumedExplicitly: false });
  useConversationsStore.setState({ viewOnly: false, sessions: [] });
  useCommandPaletteStore.setState({ open: true });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'agents.callable') {
      if (failed) throw new Error('unavailable');
      return { agents: callable, default_agent: null, unrestricted: useAuthStore.getState().user?.role === 'admin' };
    }
    if (method === 'agents.list') return { agents: roster };
    return { agents: [], sessions: [], skills: [], items: [], plans: [], tasks: [], pages: [], files: [], hits: [], routines: [], predictions: [], states: [], counts: {}, entries: [], sources: [] };
  });
});
const openFirstPicker = async () => { const picker = screen.queryAllByRole('combobox')[0]; if (picker) fireEvent.click(picker); };
const surfaces = [
  { name: 'foresight picker', render: () => <ForesightPage />, open: openFirstPicker },
  { name: 'goals picker', render: () => <GoalsPage />, open: openFirstPicker },
  { name: 'canvas picker', render: () => <CanvasPage />, open: openFirstPicker },
  { name: 'runs picker', render: () => <RunsPage />, open: openFirstPicker },
  { name: 'timeline picker', render: () => <TimelinePage />, open: openFirstPicker },
  { name: 'new plan picker', render: () => <PlansPage />, open: async () => { fireEvent.click(screen.getAllByRole('button', { name: 'New plan' })[0]); await openFirstPicker(); } },
  { name: 'automation picker', render: () => <RoutinesPage />, open: async () => { fireEvent.click(screen.getAllByRole('button', { name: 'New automation' })[0]); fireEvent.click(screen.getByText('(none)'));  } },
  { name: 'task filter', render: () => <TaskBoardPage />, open: async () => { fireEvent.click(screen.getByRole('button', { name: /Filter/ })); } },
  { name: 'skills filter', render: () => <MySkillsTab filter="" />, open: async () => { fireEvent.click(screen.getAllByRole('combobox').at(-1)!); } },
  { name: 'skill builder picker', render: () => <SkillWizard />, open: async () => {} },
  { name: 'sidebar', render: () => <SidebarProvider><AppSidebar /></SidebarProvider>, open: async () => {} },
  { name: 'conversations filter', render: () => <ConversationsPage />, open: async () => { fireEvent.click(screen.getByRole('combobox')); } },
  { name: 'command palette', render: () => <CommandPalette />, open: async () => { fireEvent.change(screen.getByRole('combobox'), { target: { value: 'Staff' } }); } },
  { name: 'assignment picker', render: () => <AgentModelPicker value="OtherStaff" onChange={vi.fn()} />, open: async () => { fireEvent.click(screen.getByRole('button')); } },
  { name: 'export picker', render: () => <DataExportButton dataset="memories" />, open: async () => { fireEvent.click(screen.getByRole('button')); } },
  { name: 'memory picker', render: () => <MemoryPage />, open: async () => { const picker = screen.queryAllByRole('combobox')[0]; if (picker) fireEvent.click(picker); } },
  { name: 'knowledge picker', render: () => <KnowledgeHubPage />, open: async () => { const picker = screen.queryAllByRole('combobox')[0]; if (picker) fireEvent.click(picker); } },
  { name: 'employee roster', render: () => <AgentsPage />, open: async () => {} },
  { name: 'files picker', render: () => <FilesPage />, open: async () => { const picker = screen.queryAllByRole('combobox')[0]; if (picker) fireEvent.click(picker); } },
];
for (const surface of surfaces) describe(surface.name, () => {
  for (const mode of ['employee', 'manager', 'admin', 'failure'] as const) it(`${mode} uses the shared visibility boundary`, async () => {
    useAuthStore.setState(s => ({ user: { ...s.user!, role: mode === 'failure' ? 'employee' : mode } }));
    failed = mode === 'failure';
    renderWithProviders(surface.render());
    if (surface.name === 'automation picker') await surface.open();
    await waitFor(() => expect(Object.values(useCallableAgentsStore.getState().entries).some(e => !e.loading)).toBe(true));
    if (surface.name !== 'automation picker') await surface.open();
    if (mode === 'admin') {
      expect(screen.getAllByText('MainStaff', { exact: false }).length).toBeGreaterThan(0);
      expect(screen.getAllByText('OtherStaff', { exact: false }).length).toBeGreaterThan(0);
    } else {
      expect(screen.queryByText('MainStaff', { exact: false })).not.toBeInTheDocument();
      expect(screen.queryByText('OtherStaff', { exact: false })).not.toBeInTheDocument();
      expect(screen.queryByText('PublicStaff', { exact: false })).not.toBeInTheDocument();
      if (mode === 'failure') expect(screen.queryByText('AllowedStaff', { exact: false })).not.toBeInTheDocument();
      else expect(screen.getAllByText('AllowedStaff', { exact: false }).length).toBeGreaterThan(0);
    }
  });
});
for (const role of ['employee', 'manager', 'admin'] as const) it(`${role} mention suggestions use selectable grants`, async () => {
  useAuthStore.setState(s => ({ user: { ...s.user!, role } }));
  useChatStore.setState({ connectionState: 'connected', resolvedAgent: { id: 'AllowedStaff', name: 'AllowedStaff', icon: '' } });
  renderWithProviders(<WebChatPage />);
  fireEvent.change(screen.getByRole('combobox'), { target: { value: '@', selectionStart: 1 } });
  await waitFor(() => expect(screen.getAllByText('AllowedStaff').length).toBeGreaterThan(0));
  await act(async () => {});
  expect(screen.queryByRole('option', { name: /MainStaff/ }) !== null).toBe(role === 'admin');
  expect(screen.queryByRole('option', { name: /PublicStaff/ }) !== null).toBe(role === 'admin');
});
it('new chat clears a stale main selection for a member', () => {
  useChatStore.setState({ selectedAgentId: 'MainStaff' });
  useConversationsStore.getState().startNew();
  expect(useChatStore.getState().selectedAgentId).toBeNull();
});
