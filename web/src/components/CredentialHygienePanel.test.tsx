import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CredentialHygienePanel } from './CredentialHygienePanel';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('<CredentialHygienePanel>', () => {
  it('renders the clean state when no plaintext credentials are found', async () => {
    mockWsClient.call.mockResolvedValue({ clean: true, count: 0, findings: [] });

    renderWithProviders(<CredentialHygienePanel onGoToAccounts={() => {}} />);

    expect(await screen.findByText('No plaintext credential residue found')).toBeInTheDocument();
    // "Clean Up" stays disabled — nothing to act on.
    expect(screen.getByRole('button', { name: 'Clean Up' })).toBeDisabled();
  });

  it('splits findings into cleanable (has an _enc twin) and manual-only rows', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'security.credential_hygiene') {
        return Promise.resolve({
          clean: false,
          count: 2,
          findings: [
            { path: 'accounts[0].oauth_token', has_enc_twin: true, severity: 'high' },
            { path: 'providers.anthropic_api_key', has_enc_twin: false, severity: 'high' },
          ],
        });
      }
      return Promise.resolve({});
    });

    renderWithProviders(<CredentialHygienePanel onGoToAccounts={() => {}} />);

    expect(await screen.findByText('accounts[0].oauth_token')).toBeInTheDocument();
    expect(screen.getByText('providers.anthropic_api_key')).toBeInTheDocument();
    expect(screen.getByText('Cleanable')).toBeInTheDocument();
    expect(screen.getByText('Manual')).toBeInTheDocument();
    // The RPC never returns a value — only paths — so nothing resembling a
    // secret literal should ever reach the DOM.
    expect(screen.queryByText(/sk-ant|sk-|ghp_|xoxb-/)).not.toBeInTheDocument();
    // A cleanable finding exists, so the button is live.
    expect(screen.getByRole('button', { name: 'Clean Up' })).toBeEnabled();
  });

  it('confirming cleanup calls security.credential_cleanup and refreshes the report', async () => {
    const user = userEvent.setup();
    let cleaned = false;
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'security.credential_hygiene') {
        return Promise.resolve(
          cleaned
            ? { clean: true, count: 0, findings: [] }
            : {
                clean: false,
                count: 1,
                findings: [
                  { path: 'accounts[0].oauth_token', has_enc_twin: true, severity: 'high' },
                ],
              },
        );
      }
      if (method === 'security.credential_cleanup') {
        cleaned = true;
        return Promise.resolve({
          cleaned: true,
          removed_paths: ['accounts[0].oauth_token'],
          backup_path: '/home/config.toml.bak.20260815T090000Z',
        });
      }
      return Promise.resolve({});
    });

    renderWithProviders(<CredentialHygienePanel onGoToAccounts={() => {}} />);

    const cleanupButton = await screen.findByRole('button', { name: 'Clean Up' });
    await waitFor(() => expect(cleanupButton).toBeEnabled());
    await user.click(cleanupButton);

    // ConfirmDialog gates the destructive action (its confirm button carries
    // a distinct label from the panel's own toolbar trigger, which is still
    // mounted behind the dialog).
    expect(await screen.findByText('Clean up plaintext credential residue?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Confirm Cleanup' }));

    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('security.credential_cleanup'),
    );
    // The panel reloads the report after cleanup, flipping back to clean.
    expect(await screen.findByText('No plaintext credential residue found')).toBeInTheDocument();
  });

  it('the rotate-guidance link calls the onGoToAccounts callback', async () => {
    const user = userEvent.setup();
    const onGoToAccounts = vi.fn();
    mockWsClient.call.mockResolvedValue({ clean: true, count: 0, findings: [] });

    renderWithProviders(<CredentialHygienePanel onGoToAccounts={onGoToAccounts} />);

    const link = await screen.findByText('Go to Accounts and sign in again');
    await user.click(link);
    expect(onGoToAccounts).toHaveBeenCalledTimes(1);
  });
});
