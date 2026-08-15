import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CredentialInventoryPanel } from './CredentialInventoryPanel';
import type { CredentialEntry } from '@/lib/api';

beforeEach(() => {
  vi.clearAllMocks();
});

function entry(over: Partial<CredentialEntry>): CredentialEntry {
  return {
    path: 'channels.telegram_bot_token',
    configured: true,
    source: 'inline',
    source_label: 'encrypted(keyfile)',
    writable: true,
    residue: false,
    ...over,
  };
}

function mockInventory(entries: CredentialEntry[]) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'security.credential_inventory') {
      return Promise.resolve({
        entries,
        total: entries.length,
        configured: entries.filter((e) => e.configured).length,
        referenced: 0,
        residue: entries.filter((e) => e.residue).length,
        plaintext: entries.filter((e) => e.source === 'legacy').length,
      });
    }
    return Promise.resolve({});
  });
}

describe('<CredentialInventoryPanel>', () => {
  it('labels each field by where its value comes from', async () => {
    mockInventory([
      entry({ path: 'channels.telegram_bot_token', source: 'inline' }),
      entry({
        path: 'channels.discord_bot_token',
        source: 'env',
        source_label: 'env:DISCORD_TOKEN',
        writable: false,
      }),
      entry({
        path: 'channels.slack_bot_token',
        source: 'legacy',
        source_label: 'plaintext(legacy)',
      }),
      entry({
        path: 'voice.stt_api_key',
        source: 'keychain',
        source_label: 'keychain:duduclaw/stt',
        writable: false,
      }),
    ]);

    renderWithProviders(<CredentialInventoryPanel />);

    expect(await screen.findByText('channels.telegram_bot_token')).toBeInTheDocument();
    expect(screen.getByText('Encrypted')).toBeInTheDocument();
    expect(screen.getByText('Env var')).toBeInTheDocument();
    expect(screen.getByText('Plaintext')).toBeInTheDocument();
    expect(screen.getByText('OS keychain')).toBeInTheDocument();
    // The non-secret reference label is shown so an operator can tell *which*
    // env var / keychain entry a field points at.
    expect(screen.getByText('env:DISCORD_TOKEN')).toBeInTheDocument();
    expect(screen.getByText('keychain:duduclaw/stt')).toBeInTheDocument();
  });

  it('flags plaintext residue and never renders anything resembling a value', async () => {
    mockInventory([
      entry({ path: 'accounts[0].oauth_token', residue: true }),
    ]);

    renderWithProviders(<CredentialInventoryPanel />);

    expect(await screen.findByText('accounts[0].oauth_token')).toBeInTheDocument();
    expect(screen.getByText('Plaintext residue')).toBeInTheDocument();
    // The RPC is built on `describe()`, which never holds a value — so no
    // credential-shaped literal can reach the DOM.
    expect(screen.queryByText(/sk-ant|sk-live|ghp_|xoxb-|sk-ant-oat/)).not.toBeInTheDocument();
  });

  it('hides unset fields until asked, so the list opens on what is real', async () => {
    mockInventory([
      entry({ path: 'channels.telegram_bot_token' }),
      entry({
        path: 'channels.wecom_secret',
        configured: false,
        source: 'unset',
        source_label: 'unset',
      }),
    ]);

    renderWithProviders(<CredentialInventoryPanel />);

    expect(await screen.findByText('channels.telegram_bot_token')).toBeInTheDocument();
    expect(screen.queryByText('channels.wecom_secret')).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Show 1 unset' }));
    expect(screen.getByText('channels.wecom_secret')).toBeInTheDocument();
  });

  it('surfaces a load failure instead of rendering a falsely empty list', async () => {
    mockWsClient.call.mockRejectedValue(new Error('config.toml parse failed'));

    renderWithProviders(<CredentialInventoryPanel />);

    expect(
      await screen.findByText('Could not load the credential list'),
    ).toBeInTheDocument();
  });
});
