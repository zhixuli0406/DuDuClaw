import { useState } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { OdooPermissionsEditor } from './OdooPermissionsEditor';
import type { OdooPermissions } from './permissions';

const policy = { models: [{ model: 'crm.lead', name: 'Leads' }], blocked_models: ['hr.employee', 'ir.model'], read_only_models: ['ir.model'], scan_available: true };
const changed = vi.fn();
function Harness({ initial }: { initial?: Partial<OdooPermissions> }) {
  const [value, setValue] = useState<OdooPermissions>({ allowed_models: [], allowed_actions: [], unblock_models: [], ...initial });
  return <OdooPermissionsEditor value={value} onChange={(next) => { setValue(next); changed(next); }} />;
}
beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'odoo.schema_scan' ? { success: true } : policy));
});

describe('Odoo permissions editor', () => {
  it('offers all verbs and saves bare and table-specific button grants separately', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['execute', 'execute:sale.order'] }} />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Add rule' })).toBeEnabled());
    const first = screen.getByRole('group', { name: 'Rule 1' });
    expect(within(first).getAllByRole('checkbox')).toHaveLength(6);
    for (const name of ['Search and read', 'Create', 'Update', 'Delete', 'Button actions', 'Other methods']) {
      expect(within(first).getByRole('checkbox', { name })).toBeInTheDocument();
    }
    await user.click(within(first).getByRole('checkbox', { name: 'Button actions' }));
    await user.click(within(screen.getByRole('group', { name: 'Rule 2' })).getByRole('checkbox', { name: 'Button actions' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).toEqual(['execute', 'action', 'execute:sale.order', 'action:sale.order']);
    await user.click(within(first).getByRole('checkbox', { name: 'Other methods' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).toEqual(['action', 'execute:sale.order', 'action:sale.order']);
  });

  it('loads existing button grants as editable checked rules', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['action', 'action:sale.order'] }} />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Add rule' })).toBeEnabled());
    expect(screen.getAllByRole('checkbox', { name: 'Button actions' }).every((box) => box.getAttribute('aria-checked') === 'true')).toBe(true);
    expect(screen.queryByText('Other settings')).not.toBeInTheDocument();
    await user.click(within(screen.getByRole('group', { name: 'Rule 2' })).getByRole('checkbox', { name: 'Button actions' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).toEqual(['action']);
  });
  it('explains empty permissions and retains configured tables when the scan fails', async () => {
    mockWsClient.call.mockImplementation((method: string) => method === 'odoo.permission_options'
      ? Promise.reject(new Error('offline')) : Promise.resolve(policy));
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_models: ['x_old.table'] }} />);
    expect(screen.getByText(/No tables selected means/)).toBeInTheDocument();
    expect(screen.getByText(/No action settings means/)).toBeInTheDocument();
    expect(await screen.findByText(/Could not retrieve tables/)).toBeInTheDocument();
    const configured = screen.getByRole('checkbox', { name: 'x_old.table' });
    expect(configured).toBeChecked();
    await user.click(configured);
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_models: [] }));
    expect(screen.getByRole('checkbox', { name: 'x_old.table' })).not.toBeChecked();
    expect(screen.getByRole('button', { name: 'Scan tables' })).toBeInTheDocument();
  });

  it('requires confirmation, preserves another use, and removes the grant with the last use', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['read:hr.employee'] }} />);
    const table = await screen.findByRole('checkbox', { name: 'hr.employee · Sensitive' });
    await user.click(table);
    expect(changed).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(table).not.toBeChecked();
    await user.click(table);
    await user.click(screen.getByRole('button', { name: 'Allow access' }));
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_models: ['hr.employee'], unblock_models: ['hr.employee'] }));
    await user.click(table);
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_models: [], unblock_models: ['hr.employee'] }));
    await user.click(screen.getByRole('button', { name: 'Remove rule 1' }));
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_actions: [], unblock_models: [] }));
  });

  it('edits rule actions, pairs search/read, marks legacy partial, and preserves unknown entries', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['read', 'unknown:legacy', 'search:crm.lead'] }} />);
    await screen.findByRole('checkbox', { name: 'hr.employee · Sensitive' });
    expect(screen.getAllByText('Partial only')).toHaveLength(2);
    const first = screen.getByRole('group', { name: 'Rule 1' });
    await user.click(within(first).getByRole('checkbox', { name: /Search and read/ }));
    await user.click(within(first).getByRole('checkbox', { name: /Search and read/ }));
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_actions: ['read', 'search', 'search:crm.lead', 'unknown:legacy'] }));
    await user.click(within(first).getByRole('checkbox', { name: 'Create' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).toContain('create');
    await user.click(screen.getByRole('button', { name: 'Remove unknown:legacy' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).not.toContain('unknown:legacy');
  });

  it('adds a rule, confirms a sensitive dropdown choice and keeps metadata read-only notes', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const add = screen.getByRole('button', { name: 'Add rule' });
    await waitFor(() => expect(add).toBeEnabled());
    await user.click(add);
    await user.click(screen.getByRole('checkbox', { name: 'Search and read' }));
    await user.click(screen.getByRole('combobox', { name: 'Table 1' }));
    await screen.findByRole('listbox');
    const options = screen.getAllByRole('option');
    expect(options[0]).toHaveTextContent('All tables');
    await user.click(screen.getByRole('option', { name: 'ir.model · Sensitive · Read-only' }));
    expect(screen.getByRole('dialog')).toHaveTextContent('remains read-only');
    await user.click(screen.getByRole('button', { name: 'Allow access' }));
    expect(changed).toHaveBeenLastCalledWith(expect.objectContaining({ allowed_actions: ['read:ir.model', 'search:ir.model'], unblock_models: ['ir.model'] }));
    await user.click(screen.getByRole('button', { name: 'Remove rule 1' }));
    expect(changed.mock.lastCall?.[0].unblock_models).toEqual([]);
  });

  it('never saves a sensitive grant for an empty draft rule', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Add rule' })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: 'Add rule' }));
    await user.click(screen.getByRole('combobox', { name: 'Table 1' }));
    await user.click(await screen.findByRole('option', { name: 'hr.employee · Sensitive' }));
    await user.click(screen.getByRole('button', { name: 'Allow access' }));
    expect(changed.mock.lastCall?.[0].unblock_models).toEqual([]);
    await user.click(screen.getByRole('checkbox', { name: 'Search and read' }));
    expect(changed.mock.lastCall?.[0]).toEqual({ allowed_models: [], allowed_actions: ['read:hr.employee', 'search:hr.employee'], unblock_models: ['hr.employee'] });
    await user.click(screen.getByRole('button', { name: 'Remove rule 1' }));
    expect(changed.mock.lastCall?.[0].unblock_models).toEqual([]);
  });

  it('keeps an empty sensitive draft when a saved partial rule is normalized', async () => {
    const user = userEvent.setup();
    const before = { allowed_models: [], allowed_actions: ['read:crm.lead'], unblock_models: [] };
    const onChange = vi.fn();
    const view = renderWithProviders(<OdooPermissionsEditor value={before} onChange={onChange} />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Add rule' })).toBeEnabled());
    await user.click(screen.getByRole('button', { name: 'Add rule' }));
    await user.click(screen.getByRole('combobox', { name: 'Table 2' }));
    await user.click(await screen.findByRole('option', { name: 'hr.employee · Sensitive' }));
    await user.click(screen.getByRole('button', { name: 'Allow access' }));
    expect(onChange.mock.lastCall?.[0].unblock_models).toEqual([]);
    view.rerender(<OdooPermissionsEditor value={{ ...before, allowed_actions: ['read:crm.lead', 'search:crm.lead'] }} onChange={onChange} />);
    expect(screen.getByRole('combobox', { name: 'Table 2' })).toHaveTextContent('hr.employee');
    expect(screen.queryByText('Partial only')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Remove rule 2' }));
    await user.click(screen.getByRole('button', { name: 'Add rule' }));
    await user.click(screen.getByRole('combobox', { name: 'Table 2' }));
    await user.click(await screen.findByRole('option', { name: 'hr.employee · Sensitive' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });

  it('loads fresh options after the existing schema scan', async () => {
    let scanned = false;
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'odoo.schema_scan') { scanned = true; return Promise.resolve({ success: true }); }
      return Promise.resolve({ ...policy, models: scanned ? policy.models : [], scan_available: scanned });
    });
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_models: ['x_old.table'] }} />);
    await user.click(screen.getByRole('button', { name: 'Scan tables' }));
    expect(await screen.findByRole('checkbox', { name: 'Leads (crm.lead)' })).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: 'x_old.table' })).toBeChecked();
    expect(mockWsClient.call).toHaveBeenCalledWith('odoo.schema_scan', {}, false, 300000);
  });
});

describe('Odoo account rights', () => {
  const rights = { ...policy, models: [{ model: 'crm.lead', name: 'Leads', access: {
    read: true, create: false, write: false, unlink: null, checked_at: '2026-11-03T00:00:00Z',
  } }] };
  beforeEach(() => mockWsClient.call.mockResolvedValue(rights));

  it('disables unavailable actions, enables unknowns and keeps saved denied actions removable', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['write:crm.lead'] }} />);
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Create' })).toHaveAttribute('aria-disabled', 'true'));
    expect(screen.getByRole('checkbox', { name: 'Other methods' })).toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByRole('checkbox', { name: 'Button actions' })).toHaveAttribute('aria-disabled', 'true');
    await user.click(screen.getByRole('checkbox', { name: 'Create' }));
    expect(changed).not.toHaveBeenCalled();
    expect(screen.getByRole('checkbox', { name: 'Delete' })).not.toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByRole('checkbox', { name: 'Update' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Update' })).not.toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByText(/Saved action unavailable/)).toBeInTheDocument();
    expect(screen.getByText('Could not confirm Odoo permissions')).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: 'Update' }));
    expect(changed.mock.lastCall?.[0].allowed_actions).toEqual([]);
    expect(screen.getByRole('checkbox', { name: 'Update' })).toHaveAttribute('aria-disabled', 'true');
  });

  it('keeps all-table actions available and explains per-table and button checks', async () => {
    renderWithProviders(<Harness initial={{ allowed_actions: ['read'] }} />);
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Create' })).not.toHaveAttribute('aria-disabled', 'true'));
    for (const checkbox of within(screen.getByRole('group', { name: 'Rule 1' })).getAllByRole('checkbox')) expect(checkbox).not.toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByText('Actual account permissions differ per table.')).toBeInTheDocument();
    expect(screen.getByText(/Button actions cover confirm, validate and archive/)).toBeInTheDocument();
  });

  it('explains that general create and update need an explicit table grant', () => {
    renderWithProviders(<Harness />);
    expect(screen.getByText(/General create and update \(for any table\) need an explicit grant/)).toBeInTheDocument();
  });

  it('reads cached rights on load and refreshes only after explicit rescan', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial={{ allowed_actions: ['read:crm.lead'] }} />);
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Create' })).toHaveAttribute('aria-disabled', 'true'));
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'odoo.schema_scan')).toHaveLength(0);
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'odoo.schema_scan'
      ? { success: true } : { ...rights, models: [{ ...rights.models[0], access: { ...rights.models[0].access, create: true } }] }));
    await user.click(screen.getByRole('button', { name: 'Rescan tables' }));
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Create' })).not.toHaveAttribute('aria-disabled', 'true'));
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'odoo.schema_scan')).toHaveLength(1);
  });
});

it('never applies global account denials to a separate employee connection', async () => {
  mockWsClient.call.mockResolvedValue({ ...policy, models: [{ model: 'crm.lead', name: 'Leads', access: {
    read: false, create: false, write: false, unlink: false, checked_at: 'now',
  } }] });
  renderWithProviders(<OdooPermissionsEditor defaultAccount={false}
    value={{ allowed_models: [], allowed_actions: ['read:crm.lead'], unblock_models: [] }} onChange={changed} />);
  await screen.findByText(/separate connection are unconfirmed/);
  await waitFor(() => expect(screen.getByRole('button', { name: 'Add rule' })).toBeEnabled());
  for (const checkbox of within(screen.getByRole('group', { name: 'Rule 1' })).getAllByRole('checkbox')) {
    expect(checkbox).not.toHaveAttribute('aria-disabled', 'true');
  }
  expect(screen.getAllByText('Could not confirm Odoo permissions')).toHaveLength(6);
});
