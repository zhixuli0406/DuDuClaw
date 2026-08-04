import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { OdooPage } from './OdooPage';

beforeEach(() => {
  vi.clearAllMocks();
  // All RPCs (odoo.status / odoo.config / agents.list) resolve through the same
  // mock — a shape that satisfies a disconnected status, an empty config and an
  // empty agent roster.
  mockWsClient.call.mockResolvedValue({ connected: false, agents: [] });
});

describe('OdooPage', () => {
  it('renders the connection settings section after load', async () => {
    renderWithProviders(<OdooPage />);

    expect(await screen.findByText('Connection Settings')).toBeInTheDocument();
  });

  it('renders the save action', async () => {
    renderWithProviders(<OdooPage />);

    expect(await screen.findByRole('button', { name: /^save$/i })).toBeInTheDocument();
  });
});

/**
 * Same class as the Google tab (WP13): a write-only credential that is on file
 * renders identically to one that never existed (`••••••••`), so the operator
 * cannot tell a saved connection from an empty form — and a blank field looks
 * like it would erase the credential, when omitting it actually keeps it.
 */
describe('OdooPage saved-credential display', () => {
  function mockConfig(extra: Record<string, unknown>) {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.config') {
        return Promise.resolve({
          url: 'https://mycompany.odoo.com',
          db: 'mycompany',
          protocol: 'jsonrpc',
          auth_method: 'api_key',
          username: 'admin@mycompany.com',
          poll_enabled: true,
          poll_interval_seconds: 60,
          poll_models: [],
          webhook_enabled: false,
          features_crm: true,
          features_sale: true,
          features_inventory: true,
          features_accounting: true,
          features_project: false,
          features_hr: false,
          ...extra,
        });
      }
      return Promise.resolve({ connected: false, agents: [] });
    });
  }

  it('says the api key is saved and that blank keeps it', async () => {
    mockConfig({ has_api_key: true });
    renderWithProviders(<OdooPage />);

    expect(
      await screen.findByPlaceholderText('Saved, leave blank to keep it'),
    ).toBeInTheDocument();
  });

  it('keeps the neutral placeholder when nothing is stored', async () => {
    mockConfig({ has_api_key: false });
    renderWithProviders(<OdooPage />);

    await screen.findByText('Connection Settings');
    expect(
      screen.queryByPlaceholderText('Saved, leave blank to keep it'),
    ).not.toBeInTheDocument();
  });
});
