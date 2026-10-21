import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { PermissionsTab, allowedRoles, isWholeChannel, ownerCount } from './PermissionsTab';
import { callsOf, collection, mockRpc, renderAt } from './test-fixtures';
import type { KbAclEntry } from '@/lib/knowledge-api';

const PRINCIPALS = {
  edition: 'enterprise',
  kinds: ['user', 'department', 'agent', 'channel'],
  users: [
    { id: 'u1', display_name: 'Me', department: null },
    { id: 'u2', display_name: 'Amy Chen', department: 'sales' },
  ],
  departments: ['sales'],
  agents: [{ id: 'helper', display_name: 'Helper', department: null }],
  channels: ['line'],
};

function setup(entries: KbAclEntry[], setAcl?: (p: Record<string, unknown>) => unknown) {
  mockRpc({
    'knowledge.acl.get': () => ({ entries, revision: 4 }),
    'knowledge.principals.search': () => PRINCIPALS,
    'knowledge.acl.set': setAcl ?? (() => ({ revision: 5 })),
  });
}

beforeEach(() => vi.clearAllMocks());

describe('PermissionsTab', () => {
  it('non-owners see an explanation, not the list', async () => {
    mockRpc({});
    renderAt(<PermissionsTab collection={collection({ my_role: 'editor' })} isOwner={false} onChanged={vi.fn()} />);
    expect(screen.getByText('Only owners manage sharing')).toBeInTheDocument();
    expect(screen.getByText(/Your role here is Editor/)).toBeInTheDocument();
    expect(callsOf('knowledge.acl.get')).toHaveLength(0);
  });

  it('limits roles by kind and protects the last owner', async () => {
    setup([
      { kind: 'user', id: 'u1', role: 'owner' },
      { kind: 'department', id: 'sales', role: 'editor' },
      { kind: 'agent', id: 'helper', role: 'viewer' },
    ]);
    renderAt(<PermissionsTab collection={collection()} isOwner onChanged={vi.fn()} />);
    const ownerSelect = await screen.findByRole('combobox', { name: 'Role for Me' });
    expect(ownerSelect).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Remove Me' })).toBeDisabled();
    const dept = screen.getByRole('combobox', { name: 'Role for sales' });
    expect(within(dept).getByRole('option', { name: 'Owner' })).toBeDisabled();
    expect(within(dept).getByRole('option', { name: 'Editor' })).not.toBeDisabled();
    const agent = screen.getByRole('combobox', { name: 'Role for Helper' });
    expect(within(agent).getByRole('option', { name: 'Editor' })).toBeDisabled();
    // Nothing changed yet: save is off.
    expect(screen.getByRole('button', { name: 'Save sharing' })).toBeDisabled();
  });

  it('adds a person, warns about a whole channel, and saves with the revision it loaded', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    setup([{ kind: 'user', id: 'u1', role: 'owner' }]);
    renderAt(<PermissionsTab collection={collection()} isOwner onChanged={onChanged} />);
    await screen.findByRole('combobox', { name: 'Role for Me' });

    await user.click(await screen.findByRole('radio', { name: 'People' }));
    await user.click(await screen.findByRole('button', { name: 'Add Amy Chen' }));
    await user.selectOptions(screen.getByRole('combobox', { name: 'Role for Amy Chen' }), 'editor');

    await user.click(screen.getByRole('radio', { name: 'Channels' }));
    await user.click(await screen.findByRole('button', { name: 'Add LINE' }));
    expect(screen.getByRole('alert')).toHaveTextContent(/including strangers/);

    await user.click(screen.getByRole('button', { name: 'Save sharing' }));
    await waitFor(() => expect(callsOf('knowledge.acl.set')).toHaveLength(1));
    expect(callsOf('knowledge.acl.set')[0]).toEqual({
      collection_id: 'col_1',
      expected_revision: 4,
      entries: [
        { kind: 'user', id: 'u1', role: 'owner' },
        { kind: 'user', id: 'u2', role: 'editor' },
        { kind: 'channel', id: 'line', role: 'viewer' },
      ],
    });
    expect(onChanged).toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Save sharing' })).toBeDisabled();
  });

  it('a stale revision asks to reload', async () => {
    const user = userEvent.setup();
    setup(
      [
        { kind: 'user', id: 'u1', role: 'owner' },
        { kind: 'agent', id: 'helper', role: 'viewer' },
      ],
      () => {
        throw new Error('權限已被其他人修改，請重新載入後再儲存。');
      },
    );
    renderAt(<PermissionsTab collection={collection()} isOwner onChanged={vi.fn()} />);
    await user.click(await screen.findByRole('button', { name: 'Remove Helper' }));
    await user.click(screen.getByRole('button', { name: 'Save sharing' }));
    expect(await screen.findByText(/Someone changed the sharing/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Reload' }));
    expect(await screen.findByRole('button', { name: 'Remove Helper' })).toBeInTheDocument();
    expect(callsOf('knowledge.acl.get')).toHaveLength(2);
  });

  it('helpers', () => {
    expect(allowedRoles('user')).toEqual(['viewer', 'editor', 'owner']);
    expect(allowedRoles('department')).toEqual(['viewer', 'editor']);
    expect(allowedRoles('agent')).toEqual(['viewer']);
    expect(allowedRoles('channel')).toEqual(['viewer']);
    expect(ownerCount([{ kind: 'user', id: 'a', role: 'owner' }, { kind: 'department', id: 'd', role: 'editor' }])).toBe(1);
    expect(isWholeChannel({ kind: 'channel', id: 'line' })).toBe(true);
    expect(isWholeChannel({ kind: 'channel', id: 'line:C1' })).toBe(false);
  });
});
