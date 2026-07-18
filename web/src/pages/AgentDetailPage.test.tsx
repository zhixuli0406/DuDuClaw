import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { AgentDetailPage } from './AgentDetailPage';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';

const DETAIL = {
  name: 'my-bot',
  display_name: 'My Bot',
  status: 'active',
  role: 'main',
  trigger: '@bot',
  archived: false,
  avatar: null,
  department: '',
  skills: ['research', 'writing'],
  model: { preferred: 'claude-sonnet', api_mode: 'cli' },
  budget: { spent_cents: 120, monthly_limit_cents: 5000 },
  heartbeat: { enabled: true },
};

function renderAt(id: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[`/agents/${id}`]}>
        <Routes>
          <Route path="/agents/:id" element={<AgentDetailPage />} />
          <Route path="/agents/:id/:tab" element={<AgentDetailPage />} />
          <Route path="/agents" element={<div>roster-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  // Combined payload: inspect reads the whole object; tasks.list / activity.list
  // pick their `.tasks` / `.events` fields off the same envelope.
  mockWsClient.call.mockResolvedValue({ ...DETAIL, tasks: [], events: [], agents: [] });
  useAgentsStore.setState({ agents: [], loading: false, loaded: true });
  useConnectionStore.setState({ state: 'authenticated' } as never);
});

describe('AgentDetailPage', () => {
  it('renders the hero header, breadcrumb, and tab strip', async () => {
    renderAt('my-bot');

    // Name renders in the hero once the inspect resolves.
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'My Bot' })).toBeInTheDocument();
    });
    // Breadcrumb root back to the roster.
    expect(screen.getAllByText('Agents').length).toBeGreaterThan(0);
    // Three-tab strip.
    expect(screen.getByRole('tab', { name: 'Overview' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Work' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Records' })).toBeInTheDocument();
  });

  it('shows the not-found state for an unknown id', async () => {
    mockWsClient.call.mockRejectedValue(new Error('nope'));
    renderAt('ghost');
    await waitFor(() => {
      expect(screen.getByText('This staff member could not be found')).toBeInTheDocument();
    });
  });
});
