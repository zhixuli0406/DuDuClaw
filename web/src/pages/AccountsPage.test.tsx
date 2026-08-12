import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AccountsPage } from './AccountsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Budget summary tolerates missing fields — {} keeps every ?? 0 fallback safe.
  mockWsClient.call.mockResolvedValue({});
});

describe('AccountsPage (MDS)', () => {
  it('renders the accounts tab description and budget KPI', async () => {
    renderWithProviders(<AccountsPage />);
    expect(await screen.findByText('Accounts & Budget')).toBeInTheDocument();
  });

  it('offers Grok in the one-click-login CLI picker and opens its login modal with the Docker caveat', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AccountsPage />);

    // 2026-08-04 (WP3): the picker's chrome is i18n'd, so this asserts the
    // English catalogue the test provider loads rather than hardcoded zh-TW.
    await user.click(screen.getByRole('button', { name: /One-click sign-in/i }));
    const grokOption = await screen.findByRole('button', { name: /Grok/ });
    await user.click(grokOption);

    // CliLoginModal opened for the grok runtime — title + Docker volume caveat.
    expect(
      await screen.findByText(/Sign in to Grok \(SuperGrok subscription\)/i),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(/duduclaw-grok volume/),
    ).toBeInTheDocument();
  });

  // WP-C (§2-2): the page used to have zero links to any AI staff member, so an
  // account could not be traced back to the work it pays for.
  it('lists the staff members whose account pool names each account', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'accounts.budget_summary') {
        return Promise.resolve({
          total_budget_cents: 5000,
          total_spent_cents: 100,
          accounts: [
            { id: 'main', auth_method: 'oauth', priority: 1, is_healthy: true, spent_this_month: 100, monthly_budget_cents: 5000 },
            { id: 'spare', auth_method: 'apikey', priority: 2, is_healthy: true, spent_this_month: 0, monthly_budget_cents: 1000 },
          ],
        });
      }
      if (method === 'agents.list') {
        return Promise.resolve({
          agents: [
            { name: 'sales', display_name: 'Sales Bot', model: { account_pool: ['main'] } },
            { name: 'ops', display_name: 'Ops Bot', model: { account_pool: [] } },
          ],
        });
      }
      return Promise.resolve({});
    });

    renderWithProviders(<AccountsPage />);

    expect(await screen.findByText('Sales Bot')).toBeInTheDocument();
    expect(screen.getAllByText('Staff members using this account')).toHaveLength(2);
    // The account nobody points at says so — it never borrows the other card's list.
    expect(
      screen.getByText(/No staff member names this account yet/),
    ).toBeInTheDocument();
    expect(screen.queryByText('Ops Bot')).not.toBeInTheDocument();
  });

  it('says the roster is unavailable rather than claiming nobody uses the account', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'accounts.budget_summary') {
        return Promise.resolve({
          total_budget_cents: 5000,
          total_spent_cents: 0,
          accounts: [
            { id: 'main', auth_method: 'oauth', priority: 1, is_healthy: true, spent_this_month: 0, monthly_budget_cents: 5000 },
          ],
        });
      }
      if (method === 'agents.list') return Promise.reject(new Error('forbidden'));
      return Promise.resolve({});
    });

    renderWithProviders(<AccountsPage />);

    expect(
      await screen.findByText('The staff roster could not be loaded, so this cannot be shown.'),
    ).toBeInTheDocument();
    expect(screen.queryByText(/No staff member names this account yet/)).not.toBeInTheDocument();
  });
});
