import { setStandardProductPolicyFixture } from '@/test/product-policy';
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

  async function submitAgentCreateFailure(message: string | Error) {
    mockWsClient.call.mockImplementation((method) => {
      if (method === 'agents.create') return Promise.reject(message);
      return Promise.resolve({
        roles: [],
        departments: [],
        models: [{ id: 'opus', label: 'Opus', type: 'cloud', provider: 'anthropic' }],
      });
    });
    renderPage();
    await waitFor(() => expect(screen.getByText('Basics')).toBeInTheDocument());

    const user = userEvent.setup();
    await user.type(screen.getByPlaceholderText('coder'), 'louis');
    await user.type(screen.getByPlaceholderText('Coder'), 'Louis');
    await user.selectOptions(screen.getByLabelText('Model'), 'opus');
    await user.click(screen.getByRole('button', { name: 'Create Agent' }));
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument());
  }

  it('keeps the localized name-format message for backend format errors', async () => {
    await submitAgentCreateFailure(
      'Agent name must be lowercase alphanumeric with hyphens, max 64 chars',
    );

    expect(screen.getByText('Failed to create agent. Please check the name format.')).toBeInTheDocument();
  });

  it('localizes duplicate-name errors from either backend wording', async () => {
    await submitAgentCreateFailure("Agent 'louis' already exists");

    expect(screen.getByText('An agent with this name already exists. Choose another name.')).toBeInTheDocument();
  });

  it('shows cloud agent-cap messages from the backend as-is', async () => {
    const message = '雲端方案最多可建立 5 位 AI 員工';
    await submitAgentCreateFailure(message);

    expect(screen.getByText(`Failed to create agent: ${message}`)).toBeInTheDocument();
    expect(screen.getByText(message)).toHaveClass('text-muted-foreground');
  });

  it('shows unknown backend errors with sanitized detail and no stack trace', async () => {
    await submitAgentCreateFailure(new Error('Policy load failed\n    at createAgent (gateway.js:42)'));

    expect(screen.getByText('Failed to create agent: Policy load failed')).toBeInTheDocument();
    expect(screen.queryByText(/at createAgent/)).not.toBeInTheDocument();
  });
});

// This suite exercises the standard installation catalog.
beforeEach(setStandardProductPolicyFixture);
