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
});
