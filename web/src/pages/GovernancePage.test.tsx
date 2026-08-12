import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GovernancePage } from './GovernancePage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
});

describe('GovernancePage', () => {
  it('renders the governance heading', () => {
    mockWsClient.call.mockResolvedValue({ policies: [] });
    renderWithProviders(<GovernancePage />);

    expect(screen.getByText('Governance')).toBeInTheDocument();
  });

  it('shows empty state when no policies', async () => {
    mockWsClient.call.mockResolvedValue({ policies: [] });
    renderWithProviders(<GovernancePage />);

    expect(await screen.findByText('No policies yet.')).toBeInTheDocument();
  });

  it('shows policies returned by the API', async () => {
    mockWsClient.call.mockResolvedValue({
      policies: [
        {
          policy_type: 'rate',
          policy_id: 'default-rate-mcp',
          agent_id: '*',
          resource: 'mcp_calls',
          limit: 200,
          window_seconds: 60,
          action_on_violation: 'reject',
        },
      ],
    });

    renderWithProviders(<GovernancePage />);

    expect(await screen.findByText('default-rate-mcp')).toBeInTheDocument();
  });

  it('opens the add-policy dialog when the add button is clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ policies: [] });

    renderWithProviders(<GovernancePage />);

    const addButton = await screen.findByRole('button', { name: /add policy/i });
    await user.click(addButton);

    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  // X03 (UX audit §3.3) — SecurityPage's RBAC card already links to
  // `/manage/governance?tab=governance`; this is the missing reverse
  // direction, surfaced while looking at permission-type policies.
  it('opens the Security page RBAC matrix from the permission filter', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ policies: [] });

    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/manage/governance']}>
          <Routes>
            <Route path="/manage/governance" element={<GovernancePage />} />
            <Route path="/manage/security" element={<div>security-probe</div>} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );

    // Hint bar is scoped to the "permission" filter.
    expect(screen.queryByRole('button', { name: 'View the current RBAC matrix' })).not.toBeInTheDocument();

    await user.click(await screen.findByRole('radio', { name: /permission/i }));
    const link = await screen.findByRole('button', { name: 'View the current RBAC matrix' });
    await user.click(link);

    await waitFor(() => {
      expect(screen.getByText('security-probe')).toBeInTheDocument();
    });
  });
});
