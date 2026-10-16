import { setStandardProductPolicyFixture } from '@/test/product-policy';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, within } from '@testing-library/react';
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

  it('does not offer Antigravity in the one-click-login CLI picker (agy has no login subcommand)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AccountsPage />);

    await user.click(screen.getByRole('button', { name: /One-click sign-in/i }));
    expect(await screen.findByRole('button', { name: /Grok/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Antigravity/i })).not.toBeInTheDocument();
  });

  it('does not offer Gemini in the one-click-login CLI picker (the Gemini CLI is deprecated)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AccountsPage />);

    await user.click(screen.getByRole('button', { name: /One-click sign-in/i }));
    expect(await screen.findByRole('button', { name: /Grok/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Gemini/i })).not.toBeInTheDocument();
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

  // Backend contract addition: `accounts.list`/`accounts.budget_summary`
  // items may carry `credential_state` — a signal independent of the
  // existing `is_healthy` rotation/cooldown badge. `ok` must stay quiet
  // (no extra badge); a broken credential must surface a clear error badge
  // with the backend's detail sentence as its tooltip.
  it('shows a credential-state badge only for the account whose credential is not ok', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'accounts.budget_summary') {
        return Promise.resolve({
          total_budget_cents: 5000,
          total_spent_cents: 0,
          accounts: [
            {
              id: 'main',
              auth_method: 'oauth',
              priority: 1,
              is_healthy: true,
              spent_this_month: 0,
              monthly_budget_cents: 5000,
              credential_state: 'ok',
            },
            {
              id: 'broken-one',
              auth_method: 'apikey',
              priority: 2,
              is_healthy: true,
              spent_this_month: 0,
              monthly_budget_cents: 1000,
              credential_state: 'broken',
              credential_detail: 'Credential probe failed: 401',
            },
          ],
        });
      }
      return Promise.resolve({});
    });

    renderWithProviders(<AccountsPage />);

    const brokenBadge = await screen.findByText('Credential broken');
    expect(brokenBadge).toBeInTheDocument();
    expect(brokenBadge).toHaveAttribute('title', 'Credential probe failed: 401');

    const mainHeading = await screen.findByText('main');
    const mainCard = mainHeading.closest('.rounded-xl');
    expect(mainCard).not.toBeNull();
    expect(within(mainCard as HTMLElement).queryByText('Credential broken')).not.toBeInTheDocument();
    expect(within(mainCard as HTMLElement).queryByText('Unverified')).not.toBeInTheDocument();
  });

  it('renders no credential-state badge when the field is absent (older gateway)', async () => {
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
      return Promise.resolve({});
    });

    renderWithProviders(<AccountsPage />);

    await screen.findByText('main');
    expect(screen.queryByText('Credential broken')).not.toBeInTheDocument();
    expect(screen.queryByText('Unverified')).not.toBeInTheDocument();
    expect(screen.queryByText('Token invalid')).not.toBeInTheDocument();
    expect(screen.queryByText('Org disabled')).not.toBeInTheDocument();
  });
});

// This suite exercises the standard installation catalog.
beforeEach(setStandardProductPolicyFixture);

// 2026-10-16 risk disclosure: the OAuth-token path stores a subscription
// credential, so it carries the same disclosure + acknowledgment as the
// one-click logins, and only then sends `accept_subscription_risk`.
describe('AccountsPage subscription risk disclosure', () => {
  async function openAddDialog(user: ReturnType<typeof userEvent.setup>) {
    renderWithProviders(<AccountsPage />);
    await user.click(screen.getByRole('button', { name: /Add account/i }));
    return screen.findByRole('dialog');
  }

  it('gates the OAuth token path of Add account behind the risk disclosure', async () => {
    const user = userEvent.setup();
    const dialog = await openAddDialog(user);

    // API key (default): no disclosure.
    expect(within(dialog).queryByTestId('subscription-risk-disclosure')).not.toBeInTheDocument();

    await user.click(within(dialog).getAllByRole('combobox')[1]);
    await user.click(await screen.findByRole('option', { name: 'OAuth Token' }));
    expect(within(dialog).getByTestId('subscription-risk-disclosure')).toBeInTheDocument();
    expect(within(dialog).getByRole('link', { name: /legal and compliance/i })).toHaveAttribute(
      'href',
      'https://code.claude.com/docs/en/legal-and-compliance',
    );

    await user.type(within(dialog).getByPlaceholderText(/e\.g\. main/i), 'sub-1');
    await user.type(within(dialog).getByPlaceholderText('oauth-token-...'), 'sk-ant-oat01-x');
    const submit = within(dialog).getByRole('button', { name: /^Add Account$/ });
    expect(submit).toBeDisabled();

    await user.click(within(dialog).getByRole('checkbox', { name: /read the policy and risks/i }));
    expect(submit).toBeEnabled();
    await user.click(submit);

    expect(mockWsClient.call).toHaveBeenCalledWith(
      'accounts.add',
      expect.objectContaining({ id: 'sub-1', type: 'oauth', accept_subscription_risk: true }),
    );
  });

  it('sends no risk flag for an API key account', async () => {
    const user = userEvent.setup();
    const dialog = await openAddDialog(user);
    await user.type(within(dialog).getByPlaceholderText(/e\.g\. main/i), 'key-1');
    await user.type(within(dialog).getByPlaceholderText(/sk-ant/i), 'sk-ant-api03-x');
    await user.click(within(dialog).getByRole('button', { name: /^Add Account$/ }));

    const call = mockWsClient.call.mock.calls.find((c) => c[0] === 'accounts.add');
    expect(call?.[1]).toMatchObject({ id: 'key-1', type: 'api_key' });
    expect(call?.[1]).not.toHaveProperty('accept_subscription_risk');
  });

  it('shows the policy note on the subscription hero card', async () => {
    renderWithProviders(<AccountsPage />);
    expect((await screen.findAllByTestId('subscription-risk-note')).length).toBeGreaterThan(0);
  });
});
