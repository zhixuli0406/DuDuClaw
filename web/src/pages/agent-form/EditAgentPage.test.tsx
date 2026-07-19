import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { SidebarProvider } from '@/components/mds';
import { EditAgentPage } from './EditAgentPage';
import { useAgentsStore } from '@/stores/agents-store';

// Keep the live model registry out of the smoke test — the ModelSelect only
// needs a stable, empty list here.
vi.mock('@/hooks/useAvailableModels', () => ({
  useAvailableModels: () => ({
    models: [],
    loading: false,
    error: null,
    discoveredAt: null,
    refreshing: false,
    refresh: vi.fn(),
  }),
}));

const DETAIL = {
  name: 'my-bot',
  display_name: 'My Bot',
  role: 'specialist',
  trigger: '@bot',
  icon: '🤖',
  reports_to: '',
  department: '',
  status: 'active',
  model: { preferred: 'claude-sonnet', api_mode: 'cli' },
  budget: { monthly_limit_cents: 5000, warn_threshold_percent: 80, hard_stop: true },
  heartbeat: { enabled: false },
  permissions: {},
  evolution: {},
  // contract.get / departments.list read off this same envelope; missing fields
  // fall back to defaults.
  must_not: [],
  must_always: [],
  departments: [],
};

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <Routes>
            <Route path="/agents/:id/edit" element={<EditAgentPage />} />
            <Route path="/agents" element={<div>roster-probe</div>} />
          </Routes>
        </SidebarProvider>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ ...DETAIL });
  useAgentsStore.setState({
    agents: [],
    loading: false,
    loaded: true,
    fetchAgents: vi.fn().mockResolvedValue(undefined),
    updateAgent: vi.fn().mockResolvedValue(undefined),
  } as never);
});

describe('EditAgentPage', () => {
  it('renders every sub-tab in the rail across both groups', async () => {
    renderAt('/agents/my-bot/edit');

    // Wait for inspect to resolve into the settings shell.
    await waitFor(() => {
      expect(screen.getByRole('tab', { name: 'General' })).toBeInTheDocument();
    });

    for (const label of [
      'Skills',
      'Tools & permissions',
      'Integrations',
      'General',
      'Model',
      'Runtime',
      'Budget',
      'Automation',
      'Advanced',
    ]) {
      expect(screen.getByRole('tab', { name: label })).toBeInTheDocument();
    }
    // Group labels present in the rail.
    expect(screen.getByText('Capabilities')).toBeInTheDocument();
    expect(screen.getByText('Settings')).toBeInTheDocument();
  });

  it('honors the ?tab= query for the active panel', async () => {
    renderAt('/agents/my-bot/edit?tab=budget');

    // The Budget sub-tab heading (SettingsTab h2) is what renders when ?tab=budget.
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Budget', level: 2 })).toBeInTheDocument();
    });
    // The default General panel heading is not mounted (Base UI unmounts inactive).
    expect(screen.queryByRole('heading', { name: 'General', level: 2 })).not.toBeInTheDocument();
  });

  it('switching the rail tab swaps the visible panel', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit');

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'General', level: 2 })).toBeInTheDocument();
    });

    await user.click(screen.getByRole('tab', { name: 'Budget' }));
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Budget', level: 2 })).toBeInTheDocument();
    });
  });

  it('a field edit auto-saves via updateAgent after the debounce', async () => {
    const user = userEvent.setup();
    const updateAgent = vi.fn().mockResolvedValue(undefined);
    useAgentsStore.setState({ updateAgent } as never);

    renderAt('/agents/my-bot/edit');

    // The display-name field is pre-populated from inspect on the General tab.
    const nameInput = await screen.findByDisplayValue('My Bot');
    await user.clear(nameInput);
    await user.type(nameInput, 'Renamed Bot');

    // No manual Save button — the ~1s debounce fires the single-flight save.
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();

    // Allow the debounce window (1s) to elapse; the save is best-effort async.
    await waitFor(
      () => {
        expect(updateAgent).toHaveBeenCalledWith(
          'my-bot',
          expect.objectContaining({ display_name: 'Renamed Bot' }),
        );
      },
      { timeout: 3000 },
    );
  });

  it('shows the not-found state when inspect fails', async () => {
    mockWsClient.call.mockRejectedValue(new Error('nope'));
    renderAt('/agents/ghost/edit');
    await waitFor(() => {
      expect(screen.getByText('This staff member could not be found')).toBeInTheDocument();
    });
  });
});
