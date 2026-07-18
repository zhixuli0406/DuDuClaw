import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
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
  // No templates / departments ⇒ the plain form path.
  mockWsClient.call.mockResolvedValue({ roles: [], departments: [] });
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
});
