import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { SourcesTab, POLL_MS } from './SourcesTab';
import { callsOf, collection, mockRpc, renderAt, source } from './test-fixtures';

beforeEach(() => vi.clearAllMocks());
afterEach(() => vi.useRealTimers());

describe('SourcesTab', () => {
  it('shows each source with its state, and a failure reason in plain words', async () => {
    mockRpc({
      'knowledge.sources.list': () => ({
        sources: [
          source(),
          source({ id: 'src_2', title: 'old.doc', status: 'failed', error_code: 'legacy_office', error_message: '這是舊版 Office 格式，請另存成 .docx。' }),
          source({ id: 'src_3', title: 'notes', kind: 'paste', status: 'queued' }),
        ],
      }),
    });
    renderAt(<SourcesTab collection={collection()} canEdit onChanged={vi.fn()} />);
    expect(await screen.findByText('leave.pdf')).toBeInTheDocument();
    expect(screen.getByText('Ready (word match)')).toBeInTheDocument();
    expect(screen.getByText('Failed')).toBeInTheDocument();
    expect(screen.getByText('這是舊版 Office 格式，請另存成 .docx。')).toBeInTheDocument();
    expect(screen.getByText('Queued')).toBeInTheDocument();
    expect(screen.getByText(/4 passages/)).toBeInTheDocument();
  });

  it('viewers get no add, read-again or delete actions but can download', async () => {
    const user = userEvent.setup();
    mockRpc({ 'knowledge.sources.list': () => ({ sources: [source()] }) });
    renderAt(<SourcesTab collection={collection({ my_role: 'viewer' })} canEdit={false} onChanged={vi.fn()} />);
    await screen.findByText('leave.pdf');
    expect(screen.queryByRole('button', { name: 'Add source' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Actions for “leave.pdf”' }));
    expect(await screen.findByRole('menuitem', { name: 'Download original' })).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Read again' })).toBeNull();
    expect(screen.queryByRole('menuitem', { name: 'Delete' })).toBeNull();
  });

  it('deletes after confirmation and reports the change upward', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    let rows = [source()];
    mockRpc({
      'knowledge.sources.list': () => ({ sources: rows }),
      'knowledge.sources.delete': () => {
        rows = [];
        return { deleted: true };
      },
    });
    renderAt(<SourcesTab collection={collection()} canEdit onChanged={onChanged} />);
    await user.click(await screen.findByRole('button', { name: 'Actions for “leave.pdf”' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(callsOf('knowledge.sources.delete')).toEqual([{ source_id: 'src_1' }]));
    expect(await screen.findByText('This collection has no documents yet')).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalled();
  });

  it('queues a read-again', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.sources.list': () => ({ sources: [source({ status: 'failed', error_message: 'x' })] }),
      'knowledge.sources.reindex': () => ({ queued: true }),
    });
    renderAt(<SourcesTab collection={collection()} canEdit onChanged={vi.fn()} />);
    await user.click(await screen.findByRole('button', { name: 'Actions for “leave.pdf”' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Read again' }));
    await waitFor(() => expect(callsOf('knowledge.sources.reindex')).toEqual([{ source_id: 'src_1' }]));
  });

  it('keeps refreshing while a source is still being read, then stops', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let status: 'extracting' | 'ready' = 'extracting';
    mockRpc({ 'knowledge.sources.list': () => ({ sources: [source({ status })] }) });
    renderAt(<SourcesTab collection={collection()} canEdit onChanged={vi.fn()} />);
    expect(await screen.findByText('Reading')).toBeInTheDocument();
    status = 'ready';
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_MS + 10);
    });
    expect(await screen.findByText('Ready (word match)')).toBeInTheDocument();
    const after = callsOf('knowledge.sources.list').length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_MS * 3);
    });
    expect(callsOf('knowledge.sources.list').length).toBe(after);
  });

  it('shows a retryable error when the list fails', async () => {
    mockRpc({});
    renderAt(<SourcesTab collection={collection()} canEdit onChanged={vi.fn()} />);
    expect(await screen.findByRole('button', { name: /retry|try again/i })).toBeInTheDocument();
  });
});

