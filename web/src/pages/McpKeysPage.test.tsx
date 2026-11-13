import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import { type McpKeyEntry, type McpKeyListResult } from '@/lib/api';
import { McpKeysPage } from './McpKeysPage';

beforeEach(() => {
  vi.clearAllMocks();
});

const DAY = 24 * 60 * 60 * 1000;

/** The backend's externally grantable list (`duduclaw_core::mcp_scopes`). */
const EFFECTIVE = ['memory:read', 'memory:write', 'wiki:read', 'wiki:write', 'messaging:send'];
/** `duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS` (never `admin`). */
const EMPLOYEE = [
  'memory:read', 'memory:write', 'wiki:read', 'wiki:write', 'messaging:send', 'identity:read',
  'odoo:read', 'odoo:write', 'odoo:execute', 'google:read', 'google:write', 'notion:read',
  'notion:write', 'github:read', 'github:write', 'fork:execute', 'os:native', 'skill:execute',
  'recording', 'mail:read', 'mail:send', 'db:read', 'files:read',
];

/** Every scope a key may still carry (stored, migrated, internal): each
 *  needs a translated description even though the picker only offers the
 *  backend's `effective_scopes`. */
const ALL_DESCRIBED_SCOPES = [
  'memory:read', 'memory:write', 'wiki:read', 'wiki:write', 'messaging:send', 'identity:read',
  'odoo:read', 'odoo:write', 'odoo:execute', 'google:read', 'google:write', 'notion:read',
  'notion:write', 'github:read', 'github:write', 'fork:execute', 'os:native', 'skill:execute',
  'recording', 'mail:read', 'mail:send', 'db:read', 'files:read', 'admin',
];

function key(overrides: Partial<McpKeyEntry> = {}): McpKeyEntry {
  return {
    id: 'mk_0123456789abcdef01234567',
    name: 'Zapier sync',
    hint: 'ddc_prod_…3f9a',
    client_id: 'zapier',
    is_external: true,
    employee_key: false,
    scopes: ['memory:read'],
    ineffective_scopes: [],
    created_by: 'u-admin',
    created_by_name: 'Ada Admin',
    created_at: new Date(Date.now() - 10 * DAY).toISOString(),
    expires_at: new Date(Date.now() + 80 * DAY).toISOString(),
    days_left: 80,
    last_used_at: null,
    disabled_at: null,
    disabled_by: null,
    disabled_by_name: null,
    status: 'active',
    expiring_soon: false,
    migrated: false,
    expiry_reviewed: true,
    ...overrides,
  };
}

function serve(list: Partial<McpKeyListResult> & { keys: McpKeyEntry[] }) {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'mcp_keys.list') {
      return {
        migration_hint: false,
        legacy_pending: 0,
        ignored_legacy_entries: [],
        effective_scopes: EFFECTIVE,
        employee_key_scopes: EMPLOYEE,
        expiry_presets_days: [30, 90, 365],
        ...list,
      };
    }
    if (method === 'mcp_oauth_grants.list') return { grants: [] };
    if (method === 'mcp_keys.create') return { success: true, key: 'ddc_prod_x', key_info: list.keys[0] ?? key(), message: '' };
    return { success: true, key_info: list.keys[0] };
  });
}

function callsTo(method: string) {
  return mockWsClient.call.mock.calls.filter((c: unknown[]) => c[0] === method);
}

describe('McpKeysPage', () => {
  it('shows the empty state when no keys exist', async () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(await screen.findByText('No MCP keys yet')).toBeInTheDocument();
  });

  it('renders the create-key action', () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(screen.getByRole('button', { name: /create key/i })).toBeInTheDocument();
  });

  // RM-157: the picker renders the backend's `effective_scopes` only — no
  // hard-coded list in the dashboard — each with its translated description.
  it('offers exactly the effective scopes from the backend', async () => {
    serve({ keys: [] });
    renderWithProviders(<McpKeysPage />);
    await waitFor(() => expect(callsTo('mcp_keys.list')).toHaveLength(1));

    fireEvent.click(screen.getByRole('button', { name: /create key/i }));

    // The dialog renders through a portal (appended to document.body), so
    // query via `screen`. `aria-label` carries the bare scope string.
    const checkboxes = await screen.findAllByRole('checkbox');
    const labels = checkboxes.map((el) => el.getAttribute('aria-label')).sort();
    expect(labels).toEqual([...EFFECTIVE].sort());
    for (const scope of ['admin', 'odoo:read', 'google:read']) {
      expect(labels).not.toContain(scope);
    }
    for (const scope of EFFECTIVE) {
      expect(screen.queryByText(`mcpKeys.scopeDesc.${scope.replace(':', '.')}`)).not.toBeInTheDocument();
    }
  });

  // Every scope still has a translated description in all three catalogues
  // (stored keys, migrated keys and the employee Google key still show them).
  it('keeps a translated description for every scope', () => {
    for (const [name, catalogue] of [['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const) {
      for (const scope of [...ALL_DESCRIBED_SCOPES, ...EMPLOYEE]) {
        const id = `mcpKeys.scopeDesc.${scope.replace(':', '.')}`;
        expect((catalogue as Record<string, string>)[id], `${name}: ${id}`).toBeTruthy();
      }
    }
  });

  it('creates an external integration key with client_id and never sends is_external', async () => {
    serve({ keys: [] });
    renderWithProviders(<McpKeysPage />);
    await waitFor(() => expect(callsTo('mcp_keys.list')).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: /create key/i }));
    fireEvent.change(await screen.findByLabelText('Name'), { target: { value: 'CRM' } });
    fireEvent.change(screen.getByLabelText('Client ID'), { target: { value: 'crm' } });
    const wiki = screen.getAllByRole('checkbox').find((el) => el.getAttribute('aria-label') === 'wiki:read');
    fireEvent.click(wiki!);
    const buttons = screen.getAllByRole('button', { name: 'Create Key' });
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() => expect(callsTo('mcp_keys.create')).toHaveLength(1));
    const params = callsTo('mcp_keys.create')[0][1] as Record<string, unknown>;
    expect(params).toMatchObject({ name: 'CRM', client_id: 'crm', scopes: ['wiki:read'], env: 'prod' });
    expect(params).not.toHaveProperty('is_external');
    expect(params).not.toHaveProperty('agent_id');
  });

  it('create dialog asks for a name and an expiry, never for a pasted key', async () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);
    fireEvent.click(screen.getByRole('button', { name: /create key/i }));
    expect(await screen.findByLabelText('Name')).toBeInTheDocument();
    expect(screen.getAllByLabelText('Expiry').length).toBeGreaterThan(0);
    expect(screen.getByText('90 days')).toBeInTheDocument();
    expect(screen.queryByPlaceholderText('ddc_prod_...')).not.toBeInTheDocument();
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
  });

  it('lists name, creator, expiry, last use and status for each key', async () => {
    serve({
      keys: [
        key(),
        key({
          id: 'mk_b', name: 'Old bot', status: 'disabled', created_by: null, created_by_name: null,
          migrated: true, expiry_reviewed: false, last_used_at: new Date().toISOString(),
        }),
        key({ id: 'mk_c', name: 'Forever', expires_at: null, days_left: null }),
      ],
    });
    renderWithProviders(<McpKeysPage />);
    expect(await screen.findByText('Zapier sync')).toBeInTheDocument();
    // Two of the three keys share a creator, so the name legitimately
    // appears once per row: assert each row on its own.
    const rowOf = (name: string) => {
      const row = screen.getByText(name).closest('[role="row"]');
      expect(row, name).not.toBeNull();
      return within(row as HTMLElement);
    };
    const zapier = rowOf('Zapier sync');
    expect(zapier.getByText('Ada Admin')).toBeInTheDocument();
    expect(zapier.getByText('Active')).toBeInTheDocument();
    expect(zapier.getByText('Not used yet')).toBeInTheDocument();
    const old = rowOf('Old bot');
    expect(old.getByText('Moved from config.toml')).toBeInTheDocument();
    expect(old.getByText('Disabled')).toBeInTheDocument();
    expect(old.getByText('Kept its pre-upgrade expiry')).toBeInTheDocument();
    expect(old.queryByText('Not used yet')).not.toBeInTheDocument();
    expect(old.queryByText('Ada Admin')).not.toBeInTheDocument();
    const forever = rowOf('Forever');
    expect(forever.getByText('Ada Admin')).toBeInTheDocument();
    expect(forever.getByText('Never')).toBeInTheDocument();
    expect(forever.getByText('Active')).toBeInTheDocument();
    expect(forever.getByText('Not used yet')).toBeInTheDocument();
    expect(screen.getAllByText('Ada Admin')).toHaveLength(2);
    // Only the hint is ever shown, never a full key.
    expect(screen.getAllByText('ddc_prod_…3f9a').length).toBeGreaterThan(0);
  });

  it('flags stored scopes that are not effective for an external key', async () => {
    serve({ keys: [key({ migrated: true, scopes: ['memory:read', 'odoo:read'], ineffective_scopes: ['odoo:read'] })] });
    renderWithProviders(<McpKeysPage />);
    expect(await screen.findByText('Not effective for an external key: odoo:read')).toBeInTheDocument();
  });

  it('shows the expiry banner for keys expiring within 7 days', async () => {
    serve({ keys: [key({ expiring_soon: true, days_left: 3 }), key({ id: 'mk_b', name: 'Later' })] });
    renderWithProviders(<McpKeysPage />);
    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent('1 key expires within 7 days: Zapier sync');
    expect(screen.getByText('3 days left')).toBeInTheDocument();
  });

  it('shows the migration hint once and dismisses it through the RPC', async () => {
    serve({ keys: [key({ migrated: true, expiry_reviewed: false })], migration_hint: true });
    renderWithProviders(<McpKeysPage />);
    expect(await screen.findByText(/moved out of config.toml/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Got it' }));
    await waitFor(() => expect(callsTo('mcp_keys.dismiss_migration_hint')).toHaveLength(1));
  });

  it('reports keys still waiting in config.toml', async () => {
    serve({ keys: [key()], legacy_pending: 2 });
    renderWithProviders(<McpKeysPage />);
    expect(await screen.findByText(/2 keys are still waiting in config.toml/)).toBeInTheDocument();
  });

  it('reports ignored config.toml entries by hint only', async () => {
    serve({ keys: [key()], ignored_legacy_entries: ['ddc_prod_…beef', 'ddc_dev_…cafe'] });
    renderWithProviders(<McpKeysPage />);
    const banner = await screen.findByText(/the key store does not know/);
    expect(banner).toHaveTextContent('2 entries');
    expect(banner).toHaveTextContent('ddc_prod_…beef');
    expect(banner).toHaveTextContent('ddc_dev_…cafe');
  });

  it('disables and enables a key in one click, by id', async () => {
    serve({ keys: [key(), key({ id: 'mk_off', name: 'Paused', status: 'disabled' })] });
    renderWithProviders(<McpKeysPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Disable Zapier sync' }));
    await waitFor(() => expect(callsTo('mcp_keys.disable')).toHaveLength(1));
    expect(callsTo('mcp_keys.disable')[0][1]).toEqual({ id: 'mk_0123456789abcdef01234567' });
    fireEvent.click(screen.getByRole('button', { name: 'Enable Paused' }));
    await waitFor(() => expect(callsTo('mcp_keys.enable')).toHaveLength(1));
    expect(callsTo('mcp_keys.enable')[0][1]).toEqual({ id: 'mk_off' });
  });

  it('deletes only after confirming, by id', async () => {
    serve({ keys: [key()] });
    renderWithProviders(<McpKeysPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Delete Zapier sync' }));
    expect(await screen.findByText(/Delete "Zapier sync"\?/)).toBeInTheDocument();
    expect(callsTo('mcp_keys.delete')).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(callsTo('mcp_keys.delete')).toHaveLength(1));
    expect(callsTo('mcp_keys.delete')[0][1]).toEqual({ id: 'mk_0123456789abcdef01234567' });
  });

  it('renames a key by id', async () => {
    serve({ keys: [key()] });
    renderWithProviders(<McpKeysPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Rename Zapier sync' }));
    const input = await screen.findByDisplayValue('Zapier sync');
    fireEvent.change(input, { target: { value: 'Zapier CRM' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(callsTo('mcp_keys.rename')).toHaveLength(1));
    expect(callsTo('mcp_keys.rename')[0][1]).toEqual({ id: 'mk_0123456789abcdef01234567', name: 'Zapier CRM' });
  });
});
