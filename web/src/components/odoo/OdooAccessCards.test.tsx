import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAgentsStore } from '@/stores/agents-store';
import { OdooAccessCards, OdooAccessNotice, useOdooAccess } from './OdooAccessCards';
import { REFUSE_GROUPS, validRefuseRule, type OdooAccess, type OdooAccessPatch } from '@/lib/odoo-access';
import type { OdooMemberCredential } from '@/lib/member-signin';

const defaultRules = REFUSE_GROUPS.flatMap(group => [...group.rules]);
let data: Extract<OdooAccess, { readable: true }>;
let unreadable: boolean;
let enabled: boolean;
let credentials: OdooMemberCredential[];
function Harness() {
  const access = useOdooAccess(true);
  return <><OdooAccessNotice access={access} /><OdooAccessCards access={access} /></>;
}
function calls(method: string) { return mockWsClient.call.mock.calls.filter(call => call[0] === method); }
async function loaded() {
  await screen.findByText('Alex AI');
}
async function confirmRisk(name: string) {
  const dialog = screen.getByRole('dialog');
  const button = within(dialog).getByRole('button', { name });
  expect(button).toBeDisabled();
  fireEvent.click(button);
  fireEvent.click(within(dialog).getByRole('checkbox'));
  fireEvent.click(button);
  await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
}

beforeEach(() => {
  vi.clearAllMocks();
  unreadable = false;
  enabled = false;
  useAgentsStore.setState({ agents: [] });
  credentials = [{ user_id: 'secret-member-id', email: 'mei@example.com', display_name: 'Mei', login: 'mei.odoo', state: 'ok', verified_at: null, last_used_at: null }];
  data = {
    readable: true,
    config: { permission_mode: 'engine', permission_mode_ack: null, company_key_agents: ['secret-agent-id'], company_key_agents_migrated: true, always_refuse: [...defaultRules], attachment_max_mb: 10, upgrade_notice_pending: true },
    defaults: { always_refuse: [...defaultRules] },
    fixed_refusals: ['Existing attachments', 'Field and access definitions', 'Generate Odoo keys', 'Generic mail', 'Internal framework methods'].map((reason, i) => ({ code: `F${i + 1}`, reason })),
    upgrade_notice_pending: true,
    employees: [{ id: 'secret-agent-id', display_name: 'Alex AI', has_own_account: false }, { id: 'secret-new-agent-id', display_name: 'Sam AI', has_own_account: true }],
    follow_mode_impact: { avatars_with_personal_credentials: 3, agents_acting_as_requester: 1 },
    risk_text: { permission_mode: 'Server mode risk', always_refuse: 'Server refusal risk', company_key_agents: 'Server company account risk' },
  };
  mockWsClient.call.mockImplementation((method: string, params: unknown) => {
    if (method === 'odoo.access_get') return Promise.resolve(unreadable ? { readable: false, message: 'Server settings are malformed' } : structuredClone(data));
    if (method === 'odoo.access_set') {
      const patch = params as OdooAccessPatch;
      const { acknowledge_risk: _ack, dismiss_upgrade_notice, ...config } = patch;
      data.config = { ...data.config, ...config };
      if (patch.permission_mode === 'follow_odoo_account') data.config.permission_mode_ack = { by: 'secret-admin-id', at: '2026-11-27T14:03:00Z' };
      if (dismiss_upgrade_notice) data.upgrade_notice_pending = false;
      return Promise.resolve(structuredClone(data));
    }
    if (method === 'users.list') return Promise.resolve({ users: [{ id: 'secret-admin-id', display_name: 'Pat Admin', email: 'pat@example.com' }] });
    if (method === 'odoo.member_credentials.list') return Promise.resolve({ credentials, enabled });
    if (method === 'odoo.member_credentials.set_enabled') return Promise.resolve({ enabled: (params as { enabled: boolean }).enabled });
    if (method === 'odoo.member_credentials.revoke') return Promise.resolve({ revoked: true });
    return Promise.resolve({});
  });
});

describe('Odoo access cards', () => {
  it('dismisses the upgrade banner with only the two flags', async () => {
    renderWithProviders(<Harness />); await loaded();
    fireEvent.click(screen.getByRole('button', { name: 'Got it' }));
    await waitFor(() => expect(calls('odoo.access_set')[0]?.[1]).toEqual({ dismiss_upgrade_notice: true, acknowledge_risk: false }));
    expect(screen.queryByText(/The company Odoo account is now/)).not.toBeInTheDocument();
  });

  it('requires acknowledgement for follow mode and shows counts, audit and resolved confirmer', async () => {
    renderWithProviders(<Harness />); await loaded();
    fireEvent.click(screen.getByRole('radio', { name: /^Follow Odoo account/ }));
    const dialog = screen.getByRole('dialog');
    for (const text of ['Skipped', 'Still enforced', 'Affected AI employees', '3 AI avatars with personal Odoo credentials', '1 AI employees using the current requester’s Odoo account']) expect(within(dialog).getByText(text)).toBeInTheDocument();
    expect(calls('odoo.access_set')).toHaveLength(0);
    await confirmRisk('Use Odoo account permissions');
    expect(calls('odoo.access_set')[0]?.[1]).toEqual({ permission_mode: 'follow_odoo_account', acknowledge_risk: true, dismiss_upgrade_notice: false });
    expect(screen.getByText(/Last confirmed by Pat Admin/)).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('secret-admin-id');
    fireEvent.click(screen.getByRole('radio', { name: /^Engine permissions/ }));
    await waitFor(() => expect(calls('odoo.access_set')[1]?.[1]).toEqual({ permission_mode: 'engine', acknowledge_risk: false, dismiss_upgrade_notice: false }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('shows the unknown impact message without invented counts', async () => {
    data.follow_mode_impact = null;
    renderWithProviders(<Harness />); await loaded();
    fireEvent.click(screen.getByRole('radio', { name: /^Follow Odoo account/ }));
    expect(screen.getByText('The number of affected AI employees cannot currently be calculated.')).toBeInTheDocument();
    expect(screen.queryByText(/AI avatars with personal Odoo credentials/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(calls('odoo.access_set')).toHaveLength(0);
  });

  it('renders missing follow acknowledgement as engine mode', async () => {
    data.config.permission_mode = 'follow_odoo_account';
    renderWithProviders(<Harness />); await loaded();
    expect(screen.getByRole('radio', { name: /^Engine permissions/ })).toBeChecked();
  });

  it('groups defaults, describes custom entries, shows all fixed refusals and hides raw rules and ids', async () => {
    data.config.always_refuse.push('write:crm.lead', 'unlink:sale.*', 'method:xyz', '*:*');
    data.config.company_key_agents.push('secret-removed-agent-id');
    renderWithProviders(<Harness />); await loaded();
    for (const text of ['Delete any record', 'Modify system models', 'Modify users', 'Modify groups', 'Two methods blocked in earlier versions (kept after upgrade)', 'Modify crm.lead', 'Delete all tables under sale.*', 'Call method xyz', 'Any action all tables', 'Unavailable AI employee']) expect(screen.getAllByText(text).length).toBeGreaterThan(0);
    for (const item of data.fixed_refusals) expect(screen.getByText(item.reason)).toBeInTheDocument();
    for (const rule of data.config.always_refuse) expect(document.body.textContent).not.toContain(rule);
    for (const id of ['secret-agent-id', 'secret-member-id', 'secret-removed-agent-id']) expect(document.body.textContent).not.toContain(id);
  });

  it('removes the whole default group with acknowledgement and a specific risk sentence', async () => {
    renderWithProviders(<Harness />); await loaded();
    const row = screen.getByText('Modify users').closest('li')!;
    fireEvent.click(within(row).getByRole('button', { name: 'Remove' }));
    expect(screen.getByText('Server refusal risk')).toBeInTheDocument();
    expect(screen.getByText(/Removing this may allow AI to modify other people/)).toBeInTheDocument();
    await confirmRisk('Remove');
    expect(calls('odoo.access_set')[0]?.[1]).toEqual({ always_refuse: defaultRules.filter(rule => !['create:res.users', 'write:res.users'].includes(rule)), acknowledge_risk: true, dismiss_upgrade_notice: false });
  });

  it('validates custom rules before adding without acknowledgement and restores defaults together', async () => {
    data.config.always_refuse = data.config.always_refuse.filter(rule => !['create:res.groups', 'write:res.groups'].includes(rule));
    renderWithProviders(<Harness />); await loaded();
    fireEvent.change(screen.getByLabelText('Action'), { target: { value: 'write' } });
    fireEvent.change(screen.getByLabelText('Table or method'), { target: { value: 'CRM..lead' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    expect(screen.getByText('Enter a valid table or method name.')).toBeInTheDocument();
    expect(calls('odoo.access_set')).toHaveLength(0);
    fireEvent.change(screen.getByLabelText('Table or method'), { target: { value: 'crm.lead' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    await waitFor(() => expect(calls('odoo.access_set')).toHaveLength(1));
    expect(calls('odoo.access_set')[0]?.[1]).toEqual({ always_refuse: [...defaultRules.filter(rule => !['create:res.groups', 'write:res.groups'].includes(rule)), 'write:crm.lead'], acknowledge_risk: false, dismiss_upgrade_notice: false });
    await waitFor(() => expect(screen.getByLabelText('Table or method')).toHaveValue(''));
    fireEvent.change(screen.getByLabelText('Add a default item'), { target: { value: 'groups' } });
    await waitFor(() => expect(calls('odoo.access_set')).toHaveLength(2));
    expect((calls('odoo.access_set')[1]?.[1] as OdooAccessPatch).always_refuse).toEqual(expect.arrayContaining(['create:res.groups', 'write:res.groups']));
    expect((calls('odoo.access_set')[1]?.[1] as OdooAccessPatch).acknowledge_risk).toBe(false);
  });

  it('adds a company AI employee with acknowledgement and removes directly', async () => {
    renderWithProviders(<Harness />); await loaded();
    fireEvent.click(screen.getByRole('button', { name: '＋ Add AI employee' }));
    expect(within(screen.getByLabelText('Choose AI employee')).queryByText('Alex AI')).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText('Choose AI employee'), { target: { value: 'secret-new-agent-id' } });
    expect(screen.getByText('Server company account risk')).toBeInTheDocument();
    await confirmRisk('Add');
    expect(calls('odoo.access_set')[0]?.[1]).toEqual({ company_key_agents: ['secret-agent-id', 'secret-new-agent-id'], acknowledge_risk: true, dismiss_upgrade_notice: false });
    fireEvent.click(screen.getByRole('button', { name: 'Remove Alex AI' }));
    await waitFor(() => expect(calls('odoo.access_set')[1]?.[1]).toEqual({ company_key_agents: ['secret-new-agent-id'], acknowledge_risk: false, dismiss_upgrade_notice: false }));
  });

  it('enables member credentials with acknowledgement and disables directly', async () => {
    renderWithProviders(<Harness />); await loaded();
    const toggle = screen.getByRole('switch');
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(calls('odoo.member_credentials.set_enabled')).toHaveLength(0);
    await confirmRisk('Enable');
    expect(calls('odoo.member_credentials.set_enabled')[0]?.[1]).toEqual({ enabled: true, acknowledge_risk: true });
    expect(toggle).toBeChecked();
    fireEvent.click(toggle);
    await waitFor(() => expect(calls('odoo.member_credentials.set_enabled')[1]?.[1]).toEqual({ enabled: false, acknowledge_risk: false }));
  });

  it('requires confirmation before revoking a credential', async () => {
    renderWithProviders(<Harness />); await loaded();
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(calls('odoo.member_credentials.revoke')).toHaveLength(0);
    expect(screen.getByText(/After revoking Mei/)).toBeInTheDocument();
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(calls('odoo.member_credentials.revoke')[0]?.[1]).toEqual({ user_id: 'secret-member-id' }));
    await screen.findByText('No members have connected their own Odoo account.');
  });

  it('disables unreadable access cards while member credentials remain available', async () => {
    unreadable = true;
    renderWithProviders(<Harness />);
    expect(await screen.findByText(/Odoo access settings cannot be read/)).toHaveTextContent('Server settings are malformed');
    expect(screen.getByRole('radio', { name: /^Follow Odoo account/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: '＋ Add AI employee' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Add' })).toBeDisabled();
    expect(screen.getByRole('switch')).not.toHaveAttribute('data-disabled');
    fireEvent.click(screen.getByRole('switch'));
    await confirmRisk('Enable');
    expect(calls('odoo.access_set')).toHaveLength(0);
    expect(calls('odoo.member_credentials.set_enabled')).toHaveLength(1);
  });

  it('keeps server refusal text visible and leaves the risk dialog open on failed save', async () => {
    renderWithProviders(<Harness />); await loaded();
    mockWsClient.call.mockRejectedValueOnce('這項設定目前不能變更');
    fireEvent.click(screen.getByRole('radio', { name: /^Follow Odoo account/ }));
    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('checkbox'));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Use Odoo account permissions' }));
    await waitFor(() => expect(within(dialog).getByRole('alert')).toHaveTextContent('這項設定目前不能變更'));
    expect(screen.getByRole('radio', { name: /^Engine permissions/, hidden: true })).toBeChecked();
  });
});

describe('account labels and availability', () => {
  it('uses agent display names from the store without exposing routing names', async () => {
    data.employees[0].display_name = null;
    useAgentsStore.setState({ agents: [{ name: 'secret-agent-id', display_name: 'Store display name' }] as never });
    renderWithProviders(<Harness />);
    expect(await screen.findByText('Store display name')).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('secret-agent-id');
  });

  it('shows all credential states and last-use timestamps', async () => {
    credentials.push({ ...credentials[0], user_id: 'secret-other-id', display_name: 'Ren', state: 'needs_reverify', last_used_at: '2026-11-20T09:31:00Z' },
      { ...credentials[0], user_id: 'secret-third-id', display_name: 'Kai', state: 'invalid' });
    renderWithProviders(<Harness />); await loaded();
    for (const status of ['Valid', 'Needs reverification', 'Invalid']) expect(screen.getByText(status)).toBeInTheDocument();
    expect(screen.getAllByText('mei.odoo', { selector: 'td' })).toHaveLength(3);
  });

  it('fails closed on access load errors without blocking the member card', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.access_get') return Promise.reject('目前無法讀取設定');
      if (method === 'odoo.member_credentials.list') return Promise.resolve({ credentials: [], enabled: false });
      return Promise.resolve({ users: [] });
    });
    renderWithProviders(<Harness />);
    expect(await screen.findByRole('alert')).toHaveTextContent('目前無法讀取設定');
    expect(screen.getByRole('radio', { name: /^Follow Odoo account/ })).toBeDisabled();
    expect(screen.getByRole('switch')).not.toHaveAttribute('data-disabled');
  });
});

describe('refusal grammar matches the server parser', () => {
  it.each(['*', 'crm.lead', 'sale.*', 'ir.model_data', '0._'])('accepts model pattern %s', target => expect(validRefuseRule('write', target)).toBe(true));
  it.each(['', ' CRM', 'CRM', 'crm..lead', '.crm', 'crm.', 'crm.*.lead', 'crm:lead', '*.*', 'crm-lead', '中文', 'crm.lead\n'])('rejects model pattern %s', target => expect(validRefuseRule('write', target)).toBe(false));
  it.each(['execute_import', 'Method_01', '_'])('accepts method %s', target => expect(validRefuseRule('method', target)).toBe(true));
  it.each(['', '*', 'action.run', 'method:name', 'run\n'])('rejects method %s', target => expect(validRefuseRule('method', target)).toBe(false));
  it('rejects unknown verbs', () => expect(validRefuseRule('read', '*')).toBe(false));
});
