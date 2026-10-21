import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { mockRpc, callsOf, source } from '@/pages/knowledge/test-fixtures';
import { SourceStatusChip, chipModel } from './SourceStatusChip';
import { PrincipalPicker, principalLabels } from './PrincipalPicker';
import { PassageDrawer } from './PassageDrawer';
import { EmbeddingStatusBanner } from './EmbeddingStatusBanner';
import type { KbPrincipals } from '@/lib/knowledge-api';

beforeEach(() => vi.clearAllMocks());

describe('SourceStatusChip', () => {
  it('maps every status to a label, keyword-only and embedding progress included', () => {
    expect(chipModel(source({ status: 'ready', status_detail: null })).labelId).toBe('kb.status.ready');
    expect(chipModel(source({ status: 'ready', status_detail: 'keyword_only' })).labelId).toBe('kb.status.readyKeywordOnly');
    expect(chipModel(source({ status: 'failed' })).tone).toBe('bad');
    expect(chipModel(source({ status: 'awaiting_approval' })).labelId).toBe('kb.status.awaitingApproval');
    const m = chipModel(source({ status: 'embedding', job: { kind: 'embed', state: 'running', attempts: 1, progress_done: 3, progress_total: 4 } }));
    expect(m.values).toEqual({ pct: 75 });
  });

  it('renders the progress percentage', () => {
    renderWithProviders(
      <SourceStatusChip source={source({ status: 'embedding', job: { kind: 'embed', state: 'running', attempts: 1, progress_done: 1, progress_total: 2 } })} />,
    );
    expect(screen.getByText('Analysing meaning 50%')).toBeInTheDocument();
  });
});

const ENTERPRISE: KbPrincipals = {
  edition: 'enterprise',
  kinds: ['user', 'department', 'agent', 'channel'],
  users: [{ id: 'u2', display_name: 'Amy Chen', department: 'sales' }],
  departments: ['sales'],
  agents: [{ id: 'helper', display_name: 'Helper', department: null }],
  channels: ['line', 'telegram'],
};

describe('PrincipalPicker', () => {
  it('Personal edition offers only AI agents and channels', async () => {
    mockRpc({
      'knowledge.principals.search': () => ({ ...ENTERPRISE, edition: 'personal', kinds: ['agent', 'channel'], users: [], departments: [] }),
    });
    renderWithProviders(<PrincipalPicker collectionId="col_1" existing={[]} onPick={vi.fn()} />);
    const group = await screen.findByRole('radiogroup', { name: 'Who' });
    const kinds = within(group).getAllByRole('radio').map((r) => r.textContent);
    expect(kinds).toEqual(['AI agents', 'Channels']);
  });

  it('adds a person, marks existing grants and composes a single-chat channel grant', async () => {
    const user = userEvent.setup();
    const onPick = vi.fn();
    mockRpc({ 'knowledge.principals.search': () => ENTERPRISE });
    renderWithProviders(
      <PrincipalPicker collectionId="col_1" existing={[{ kind: 'agent', id: 'helper', role: 'viewer' }]} onPick={onPick} />,
    );
    // Default kind is AI agents; the existing grant is shown as added.
    expect(await screen.findByRole('button', { name: 'Add Helper' })).toBeDisabled();

    await user.click(screen.getByRole('radio', { name: 'People' }));
    // People are searched, never listed.
    expect(screen.getByText('Type at least 2 characters to find a colleague.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add Amy Chen' })).toBeNull();
    await user.type(screen.getByRole('textbox', { name: 'Search by name' }), 'am');
    await user.click(await screen.findByRole('button', { name: 'Add Amy Chen' }));
    await user.clear(screen.getByRole('textbox', { name: 'Search by name' }));
    expect(onPick).toHaveBeenCalledWith({ kind: 'user', id: 'u2', label: 'Amy Chen' });

    await user.click(screen.getByRole('radio', { name: 'Channels' }));
    await user.type(screen.getByLabelText('Only one chat (optional)'), 'C42');
    await user.click(screen.getByRole('button', { name: 'Add LINE' }));
    expect(onPick).toHaveBeenLastCalledWith({ kind: 'channel', id: 'line:C42', label: 'LINE · C42' });
  });

  it('searches as the owner types (debounced) and reports a failed lookup', async () => {
    const user = userEvent.setup();
    mockRpc({ 'knowledge.principals.search': () => ENTERPRISE });
    renderWithProviders(<PrincipalPicker collectionId="col_1" existing={[]} onPick={vi.fn()} />);
    await screen.findByRole('button', { name: 'Add Helper' });
    await user.type(screen.getByRole('textbox', { name: 'Search by name' }), 'amy');
    await waitFor(() => expect(callsOf('knowledge.principals.search').some((p) => p.query === 'amy')).toBe(true));

    vi.clearAllMocks();
    mockRpc({});
    renderWithProviders(<PrincipalPicker collectionId="col_2" existing={[]} onPick={vi.fn()} />);
    expect(await screen.findByText('Could not load who you can share with')).toBeInTheDocument();
  });

  it('principalLabels maps ids to display names', () => {
    const m = principalLabels(ENTERPRISE);
    expect(m.get('user:u2')).toBe('Amy Chen');
    expect(m.get('agent:helper')).toBe('Helper');
    expect(m.get('department:sales')).toBe('sales');
    expect(principalLabels({ ...ENTERPRISE, users: [], resolved_users: [{ id: 'u7', display_name: 'Owner', department: null }] }).get('user:u7')).toBe('Owner');
  });
});

describe('PassageDrawer', () => {
  it('highlights the passage between its context and walks to the next one', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.passage.get': (p) =>
        p.chunk_id === 7
          ? { chunk_id: 7, source_id: 's', collection_id: 'c', title: 'leave.pdf', locator_text: 'page 2', heading_path: 'Leave', segment_label: null, before: 'Before. ', match: 'Annual leave is 14 days.', after: ' After.', prev_chunk_id: null, next_chunk_id: 8, suspect: false }
          : { chunk_id: 8, source_id: 's', collection_id: 'c', title: 'leave.pdf', locator_text: 'page 3', heading_path: '', segment_label: null, before: '', match: 'Sick leave.', after: '', prev_chunk_id: 7, next_chunk_id: null, suspect: true },
    });
    renderWithProviders(<PassageDrawer chunkId={7} onClose={vi.fn()} />);
    const mark = await screen.findByText('Annual leave is 14 days.');
    expect(mark.tagName).toBe('MARK');
    expect(screen.getByText('page 2 · Leave')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Previous/ })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: /Next/ }));
    expect(await screen.findByText('Sick leave.')).toBeInTheDocument();
    expect(screen.getByText(/reads like instructions/)).toBeInTheDocument();
  });

  it('shows an error when the passage may no longer be read', async () => {
    mockRpc({ 'knowledge.passage.get': () => { throw new Error('permission denied'); } });
    renderWithProviders(<PassageDrawer chunkId={9} onClose={vi.fn()} />);
    expect(await screen.findByText('This passage could not be opened')).toBeInTheDocument();
  });
});

describe('EmbeddingStatusBanner', () => {
  it('explains word matching while meaning search is off, and hides otherwise', async () => {
    mockRpc({ 'knowledge.embedding.status': () => ({ provider: 'none', available: false, mode: 'keyword_only', reason: 'x', coverage: 0 }) });
    const { unmount } = renderWithProviders(<EmbeddingStatusBanner />);
    expect(await screen.findByRole('note')).toHaveTextContent(/matches words/);
    unmount();
    mockRpc({ 'knowledge.embedding.status': () => ({ provider: 'local', available: true, mode: 'hybrid', reason: null, coverage: 1 }) });
    renderWithProviders(<EmbeddingStatusBanner />);
    await waitFor(() => expect(callsOf('knowledge.embedding.status').length).toBeGreaterThan(1));
    expect(screen.queryByRole('note')).toBeNull();
  });
});
