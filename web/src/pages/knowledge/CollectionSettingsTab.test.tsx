import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { useSystemStore } from '@/stores/system-store';
import { CollectionSettingsTab, settingsPatch } from './CollectionSettingsTab';
import { callsOf, collection, mockRpc, renderAt } from './test-fixtures';

beforeEach(() => {
  vi.clearAllMocks();
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } } as never);
});

describe('CollectionSettingsTab', () => {
  it('saves only what changed, with the timestamp it loaded', async () => {
    const user = userEvent.setup();
    const onSaved = vi.fn();
    mockRpc({ 'knowledge.collections.update': (p) => ({ collection: { ...collection(), name: p.name, updated_at: 'later' } }) });
    renderAt(<CollectionSettingsTab collection={collection()} onSaved={onSaved} onDeleted={vi.fn()} />);
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save).toBeDisabled();
    await user.clear(screen.getByLabelText('Name'));
    await user.type(screen.getByLabelText('Name'), 'HR');
    await user.click(screen.getByRole('switch'));
    await user.click(save);
    await waitFor(() =>
      expect(callsOf('knowledge.collections.update')).toEqual([
        { id: 'col_1', expected_updated_at: '2026-10-20T08:00:00.000Z', name: 'HR', embedding_policy: 'local_only' },
      ]),
    );
    expect(onSaved).toHaveBeenCalledWith(expect.objectContaining({ name: 'HR', source_count: 2 }));
  });

  it('deleting needs the collection name typed', async () => {
    const user = userEvent.setup();
    const onDeleted = vi.fn();
    mockRpc({ 'knowledge.collections.delete': () => ({ deleted: true }) });
    renderAt(<CollectionSettingsTab collection={collection()} onSaved={vi.fn()} onDeleted={onDeleted} />);
    await user.click(screen.getByRole('button', { name: 'Delete this collection' }));
    const dialog = await screen.findByRole('dialog');
    const confirm = within(dialog).getByRole('button', { name: 'Delete' });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByRole('textbox'), 'HR handbook');
    await user.click(confirm);
    await waitFor(() => expect(callsOf('knowledge.collections.delete')).toEqual([{ id: 'col_1', confirm_name: 'HR handbook' }]));
    expect(onDeleted).toHaveBeenCalled();
  });

  it('Personal edition hides the department field', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } } as never);
    mockRpc({});
    renderAt(<CollectionSettingsTab collection={collection()} onSaved={vi.fn()} onDeleted={vi.fn()} />);
    expect(screen.queryByLabelText('Department (optional)')).toBeNull();
  });

  it('settingsPatch', () => {
    const c = collection({ department: 'hr' });
    const base = { name: c.name, description: c.description, department: 'hr', sensitivity: c.sensitivity, localOnly: false };
    expect(settingsPatch(c, base)).toEqual({});
    expect(settingsPatch(c, { ...base, department: '' })).toEqual({ department: '' });
    expect(settingsPatch(c, { ...base, sensitivity: 'restricted' })).toEqual({ sensitivity: 'restricted' });
  });
});
