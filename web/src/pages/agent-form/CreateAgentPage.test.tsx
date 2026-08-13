import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { SidebarProvider } from '@/components/mds';
import { CreateAgentPage } from './CreateAgentPage';
import { useAgentsStore } from '@/stores/agents-store';

function renderPage() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/agents/new']}>
        <SidebarProvider>
          <Routes>
            <Route path="/agents/new" element={<CreateAgentPage />} />
            <Route path="/agents" element={<div>roster-probe</div>} />
          </Routes>
        </SidebarProvider>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  // No templates / departments ⇒ the plain form path. Also seed a couple of
  // models so the model picker has choices (a single combined object satisfies
  // every RPC this page fires: roles/departments + models.list).
  mockWsClient.call.mockResolvedValue({
    roles: [],
    departments: [],
    models: [
      { id: 'opus', label: 'Opus', type: 'cloud', provider: 'anthropic' },
      { id: 'grok-4.6', label: 'Grok 4.6', type: 'cloud', provider: 'xai' },
    ],
  });
  useAgentsStore.setState({
    agents: [],
    loading: false,
    loaded: true,
    fetchAgents: vi.fn().mockResolvedValue(undefined),
  } as never);
});

describe('CreateAgentPage', () => {
  it('renders the basics form and the create action', async () => {
    renderPage();

    await waitFor(() => {
      expect(screen.getByText('Basics')).toBeInTheDocument();
    });
    // Id + display-name inputs (plain form, no template selected).
    expect(screen.getByPlaceholderText('coder')).toBeInTheDocument();
    expect(screen.getByPlaceholderText('Coder')).toBeInTheDocument();
    // Create button lives in the breadcrumb header actions.
    expect(screen.getByRole('button', { name: 'Create Agent' })).toBeInTheDocument();
    // Organization section rail.
    expect(screen.getByText('Organization')).toBeInTheDocument();
  });

  it('surfaces a model picker on the blank path and requires an explicit choice', async () => {
    renderPage();
    await waitFor(() => expect(screen.getByText('Basics')).toBeInTheDocument());

    // The model picker is present (no silent server-side default any more).
    const modelSelect = screen.getByLabelText('Model');
    expect(modelSelect).toBeInTheDocument();

    const createBtn = screen.getByRole('button', { name: 'Create Agent' });
    const user = userEvent.setup();
    // Fill required id + display name but leave the model unchosen.
    await user.type(screen.getByPlaceholderText('coder'), 'coder');
    await user.type(screen.getByPlaceholderText('Coder'), 'Coder');
    // Still disabled: a model must be picked before an employee can be created.
    expect(createBtn).toBeDisabled();
  });

  it('passes the chosen model as model_preferred to agents.create', async () => {
    renderPage();
    await waitFor(() => expect(screen.getByText('Basics')).toBeInTheDocument());

    const user = userEvent.setup();
    await user.type(screen.getByPlaceholderText('coder'), 'coder');
    await user.type(screen.getByPlaceholderText('Coder'), 'Coder');
    await user.selectOptions(screen.getByLabelText('Model'), 'opus');

    const createBtn = screen.getByRole('button', { name: 'Create Agent' });
    expect(createBtn).toBeEnabled();
    await user.click(createBtn);

    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'agents.create',
        expect.objectContaining({ name: 'coder', model_preferred: 'opus' }),
      ),
    );
  });
});
