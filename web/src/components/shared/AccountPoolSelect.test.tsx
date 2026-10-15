import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AccountPoolSelect } from './AccountPoolSelect';

beforeEach(() => {
  vi.clearAllMocks();
});

/** Two accounts, one of each auth method — the shape `accounts.budget_summary` returns. */
function mockAccounts() {
  mockWsClient.call.mockResolvedValue({
    total_budget_cents: 6000,
    total_spent_cents: 0,
    accounts: [
      { id: 'main', label: 'Main subscription', auth_method: 'oauth', priority: 1, is_healthy: true, spent_this_month: 0, monthly_budget_cents: 5000 },
      { id: 'spare', label: '', auth_method: 'apikey', priority: 2, is_healthy: true, spent_this_month: 0, monthly_budget_cents: 1000 },
    ],
  });
}

describe('AccountPoolSelect', () => {
  it('picks account ids from the live account list instead of free text', async () => {
    const user = userEvent.setup();
    mockAccounts();
    const onChange = vi.fn();
    renderWithProviders(<AccountPoolSelect values={[]} onChange={onChange} />);

    await user.click(await screen.findByRole('button', { name: /Choose accounts/ }));
    await user.click(await screen.findByText('Main subscription'));

    expect(onChange).toHaveBeenCalledWith(['main']);
  });

  it('falls back to the free-text editor when the account list is unavailable', async () => {
    mockWsClient.call.mockRejectedValue(new Error('nope'));
    renderWithProviders(<AccountPoolSelect values={['legacy-id']} onChange={vi.fn()} />);

    // Degraded, not broken: the stored value stays editable by hand.
    expect(
      await screen.findByText('The account list could not be loaded, so names are typed by hand for now.'),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Choose accounts/ })).not.toBeInTheDocument();
  });

  it('flags a stored id with no live account behind it instead of dropping it', async () => {
    mockAccounts();
    renderWithProviders(<AccountPoolSelect values={['main', 'deleted-account']} onChange={vi.fn()} />);

    // Both chips survive; only the orphan carries the warning.
    expect(await screen.findByRole('button', { name: 'remove deleted-account' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'remove main' })).toBeInTheDocument();
    expect(
      screen.getByText(/One of these accounts no longer exists/),
    ).toBeInTheDocument();
  });

  it('shows the subscription risk note only when a subscription account is in the pool', async () => {
    mockAccounts();
    const { unmount } = renderWithProviders(<AccountPoolSelect values={['spare']} onChange={vi.fn()} />);
    expect(await screen.findByRole('button', { name: 'remove spare' })).toBeInTheDocument();
    expect(screen.queryByTestId('subscription-risk-note')).not.toBeInTheDocument();
    unmount();

    mockAccounts();
    renderWithProviders(<AccountPoolSelect values={['main']} onChange={vi.fn()} />);
    const note = await screen.findByTestId('subscription-risk-note');
    expect(note).toHaveTextContent(/doesn't permit third-party products/i);
    expect(screen.getByRole('link', { name: /read the policy/i })).toHaveAttribute(
      'href',
      'https://code.claude.com/docs/en/legal-and-compliance',
    );
  });
});
