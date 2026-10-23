import { describe, it, expect, vi, afterEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import en from '@/i18n/en.json';
import { renderWithProviders } from '@/test/render';

vi.mock('@/lib/ws-client', () => ({
  client: { call: vi.fn().mockResolvedValue({}), subscribe: vi.fn() },
}));

import { client } from '@/lib/ws-client';
import { HistoryDrawer, sourceLabelKey } from './HistoryDrawer';

const versions = [
  { version_id: '1730000000002-bbbbbbbbbbbb', file: 'soul', at: '2026-10-22T03:00:00Z', user_id: 'u-uuid-1', user_name: 'Alice', confirm_reason: '客服語氣調整', source: 'dashboard', sha256: 'h2', size: 10, verdict: 'pass', reason_categories: [] },
  { version_id: '1730000000001-aaaaaaaaaaaa', file: 'soul', at: '2026-10-21T03:00:00Z', user_id: 'system', source: 'initial', sha256: 'h1', size: 8, verdict: null, reason_categories: [] },
];

function mockCalls() {
  vi.mocked(client.call).mockImplementation(async (method: string, params?: Record<string, unknown>) => {
    if (method === 'agent_files.history') return { agent_id: 'a1', file: 'soul', versions };
    if (method === 'agent_files.get') return { content: `內容 ${String(params?.version_id)}`, hash: 'x', exists: true, masked: [], drift: { state: 'none', accepted_version_id: null } };
    if (method === 'agent_files.diff') return { added: 1, removed: 2, hunks: [{ op: 'add', lines: ['原本的一行'] }] };
    return {};
  });
}

function renderDrawer(canRestore: boolean, onRestore = vi.fn()) {
  renderWithProviders(
    <HistoryDrawer
      open
      onOpenChange={vi.fn()}
      agentId="a1"
      file="soul"
      fileLabel="Personality"
      currentHash="h2"
      canRestore={canRestore}
      onRestore={onRestore}
    />,
  );
  return onRestore;
}

describe('<HistoryDrawer>', () => {
  afterEach(() => vi.mocked(client.call).mockReset());

  it('says "a deleted user" instead of showing a raw id', async () => {
    vi.mocked(client.call).mockImplementation(async (method: string) =>
      method === 'agent_files.history' ? { agent_id: 'a1', file: 'soul', versions: [{ ...versions[0], user_name: null }] } : {},
    );
    renderDrawer(false);
    const row = await screen.findByTestId('history-version');
    expect(within(row).getByText(`Changed by ${en['agentFiles.history.deletedUser']}`)).toBeInTheDocument();
    expect(screen.queryByText(/u-uuid-1/)).toBeNull();
  });

  it('lists versions with who and where in plain words', async () => {
    mockCalls();
    renderDrawer(true);
    const rows = await screen.findAllByTestId('history-version');
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText(en['agentFiles.history.source.dashboard'])).toBeInTheDocument();
    expect(within(rows[0]).getByText(en['agentFiles.history.current'])).toBeInTheDocument();
    expect(within(rows[0]).getByText('Changed by Alice')).toBeInTheDocument();
    expect(within(rows[0]).getByTestId('history-reason')).toHaveTextContent('Reason: 客服語氣調整');
    expect(within(rows[1]).queryByTestId('history-reason')).toBeNull();
    expect(screen.queryByText(/u-uuid-1/)).toBeNull();
    expect(within(rows[1]).getByText(en['agentFiles.history.source.initial'])).toBeInTheDocument();
    expect(within(rows[1]).getByText('Changed by System')).toBeInTheDocument();
    expect(vi.mocked(client.call)).toHaveBeenCalledWith('agent_files.history', { agent_id: 'a1', file: 'soul' });
  });

  it('views, compares and restores a version', async () => {
    mockCalls();
    const onRestore = renderDrawer(true);
    const rows = await screen.findAllByTestId('history-version');
    // The current version cannot be compared with itself or restored.
    expect(within(rows[0]).getByRole('button', { name: en['agentFiles.history.restore'] })).toBeDisabled();
    await userEvent.click(within(rows[1]).getByRole('button', { name: en['agentFiles.history.view'] }));
    expect(await within(rows[1]).findByTestId('history-content')).toHaveTextContent('內容 1730000000001-aaaaaaaaaaaa');
    await userEvent.click(within(rows[1]).getByRole('button', { name: en['agentFiles.history.compare'] }));
    expect(await within(rows[1]).findByText('Restoring this version adds 1 lines and removes 2 lines.')).toBeInTheDocument();
    expect(vi.mocked(client.call)).toHaveBeenCalledWith('agent_files.diff', { agent_id: 'a1', file: 'soul', version_id: '1730000000001-aaaaaaaaaaaa' });
    await userEvent.click(within(rows[1]).getByRole('button', { name: en['agentFiles.history.restore'] }));
    expect(onRestore).toHaveBeenCalledWith(expect.objectContaining({ version_id: '1730000000001-aaaaaaaaaaaa' }));
  });

  it('hides restore for someone who may not edit', async () => {
    mockCalls();
    renderDrawer(false);
    await screen.findAllByTestId('history-version');
    expect(screen.queryByRole('button', { name: en['agentFiles.history.restore'] })).not.toBeInTheDocument();
  });

  it('says so when the versions cannot be loaded', async () => {
    vi.mocked(client.call).mockRejectedValue(new Error('permission denied'));
    renderDrawer(true);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent(en['agentFiles.history.loadFailed']));
  });

  it('maps unknown sources to "other"', () => {
    expect(sourceLabelKey('something-new')).toBe('agentFiles.history.source.other');
    expect(sourceLabelKey('approval')).toBe('agentFiles.history.source.approval');
  });
});
