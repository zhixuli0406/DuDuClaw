import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { useAuthStore } from '@/stores/auth-store';
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

  it('shows the internal-network switch off by default and on when stored', async () => {
    mockConfig({});
    const first = renderWithProviders(<OdooPage />);
    const off = await screen.findByRole('switch', { name: /Allow Odoo on an internal network/i });
    expect(off).toHaveAttribute('aria-checked', 'false');
    first.unmount();

    mockConfig({ allow_private_network: true });
    renderWithProviders(<OdooPage />);
    const on = await screen.findByRole('switch', { name: /Allow Odoo on an internal network/i });
    expect(on).toHaveAttribute('aria-checked', 'true');
  });

  it('fills the form from the saved config even when the live status call never answers', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.status') return new Promise(() => {}); // unreachable Odoo
      if (method === 'odoo.config') {
        return Promise.resolve({
          url: 'https://erp.example.com',
          db: 'prod',
          protocol: 'jsonrpc',
          auth_method: 'api_key',
          username: 'bot',
          poll_enabled: false,
          poll_interval_seconds: 60,
          poll_models: [],
          webhook_enabled: false,
          allow_private_network: true,
          features_crm: true,
          features_sale: true,
          features_inventory: true,
          features_accounting: true,
          features_project: false,
          features_hr: false,
        });
      }
      return Promise.resolve({ agents: [] });
    });
    renderWithProviders(<OdooPage />);
    expect(await screen.findByDisplayValue('https://erp.example.com')).toBeInTheDocument();
    expect(screen.getByDisplayValue('prod')).toBeInTheDocument();
    const sw = screen.getByRole('switch', { name: /Allow Odoo on an internal network/i });
    expect(sw).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByRole('button', { name: /^save$/i })).not.toBeDisabled();
  });

  it('blocks saving (and says why) when the saved config cannot be loaded', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.config') return Promise.reject(new Error('boom'));
      return Promise.resolve({ connected: false, agents: [] });
    });
    renderWithProviders(<OdooPage />);
    expect(await screen.findByText(/Could not load the saved Odoo settings/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^save$/i })).toBeDisabled();
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

/**
 * G4 (2026-09 feature audit): the polling switch now really starts a
 * background task against the customer's ERP. Before the poller existed the
 * form defaulted it to ON, which was harmless only because nothing consumed
 * the setting — so this pins the default to OFF from the UI side, matching
 * `OdooConfig::default()` and `odoo.configure`'s own absent-param default.
 */
describe('OdooPage polling default', () => {
  it('renders the polling switch OFF when the backend has no value for it', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.config') {
        // A config that predates the polling keys entirely.
        return Promise.resolve({ url: '', db: '', protocol: 'jsonrpc' });
      }
      return Promise.resolve({ connected: false, agents: [] });
    });
    renderWithProviders(<OdooPage />);

    const toggle = await screen.findByRole('switch', { name: 'Enable polling sync' });
    expect(toggle).toHaveAttribute('aria-checked', 'false');
  });

  it('still reflects an explicitly enabled backend value', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.config') {
        return Promise.resolve({ url: '', db: '', poll_enabled: true });
      }
      return Promise.resolve({ connected: false, agents: [] });
    });
    renderWithProviders(<OdooPage />);

    const toggle = await screen.findByRole('switch', { name: 'Enable polling sync' });
    expect(toggle).toHaveAttribute('aria-checked', 'true');
  });
});


describe('OdooPage schema snapshot', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { id: 'admin', email: 'admin@test', display_name: 'Admin', role: 'admin', status: 'active' } });
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.config') return Promise.resolve({ url: 'https://erp.example.com', db: 'prod' });
      if (method === 'odoo.status') return Promise.resolve({ connected: true, version: '20.0', schema: {
        snapshot: { scanned_at: '2026-10-28T01:00:00Z', server_version: '19.0', model_count: 4, field_count: 30, truncated: true },
        last_error: null,
      } });
      if (method === 'odoo.schema_scan') return Promise.resolve({ success: true, diff: { added_models: ['x_new'], removed_models: [], added_fields: [], removed_fields: [], version_changed: true }, schema: {
        snapshot: { scanned_at: '2026-10-28T02:00:00Z', server_version: '20.0', model_count: 5, field_count: 35, truncated: false }, last_error: null,
      } });
      return Promise.resolve({ agents: [] });
    });
  });

  it('shows snapshot version and truncation, then scans through the admin RPC', async () => {
    renderWithProviders(<OdooPage />);
    expect(await screen.findByText(/19.0/)).toBeInTheDocument();
    expect(screen.getByText(/re-scan recommended/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Re-scan Odoo structure' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('odoo.schema_scan', {}, false, 300000));
    expect(await screen.findByText(/Added models: 1/)).toBeInTheDocument();
  });

  it('does not offer the scan action to employees', async () => {
    useAuthStore.setState({ user: { id: 'staff', email: 'staff@test', display_name: 'Staff', role: 'employee', status: 'active' } });
    renderWithProviders(<OdooPage />);
    await screen.findByText('Connection Settings');
    expect(screen.queryByRole('button', { name: 'Re-scan Odoo structure' })).not.toBeInTheDocument();
  });
});
