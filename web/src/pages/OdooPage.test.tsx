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

/**
 * The per-AI-staff summary (AgentOdooOverride) used to flag `unblock_models`
 * with an inline "not yet editable from any page" note — that gap is now
 * closed on EditAgentPage.tsx's integration tab, so the note should be gone
 * and the field should read like every other row in the summary.
 */
describe('OdooPage per-agent override summary — unblock_models edit path', () => {
  function mockAgentOverride(cfgExtra: Record<string, unknown> = {}) {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [{ name: 'bot-1', display_name: 'Bot One' }] });
      }
      if (method === 'odoo.agent_config_get') {
        return Promise.resolve({
          agent_id: 'bot-1',
          configured: true,
          allowed_models: [],
          unblock_models: ['res.partner'],
          allowed_actions: [],
          company_ids: [],
          api_key_set: false,
          password_set: false,
          ...cfgExtra,
        });
      }
      // odoo.status / odoo.config for the page-level connection section.
      return Promise.resolve({ connected: false });
    });
  }

  it('shows the unblock_models value without the stale no-edit-path note', async () => {
    mockAgentOverride();
    renderWithProviders(<OdooPage />);

    expect(await screen.findByText('res.partner')).toBeInTheDocument();
    expect(screen.queryByText('Not yet editable from any page.')).not.toBeInTheDocument();
  });

  it('still cross-links to the employee edit page (the one real edit surface)', async () => {
    mockAgentOverride();
    renderWithProviders(<OdooPage />);

    // Wait for the per-agent config fetch to settle (agents.list → setSelected
    // → odoo.agent_config_get is a multi-hop async chain) before querying for
    // the cross-link that renders alongside it.
    await screen.findByText('res.partner');
    expect(
      screen.getByRole('button', { name: "Edit on this employee's page" }),
    ).toBeInTheDocument();
  });
});
