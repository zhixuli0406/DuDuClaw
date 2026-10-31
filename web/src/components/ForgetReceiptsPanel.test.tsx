import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { ForgetReceiptsPanel } from './ForgetReceiptsPanel';

const receipt = {
  id: 'receipt-1', batch_id: 'batch-1', connector_ref: 'google:home',
  display_name: 'Company Google', completed_at: '2026-10-30T09:00:00Z', approved_by: 'admin-1',
  counts: [{ agent_id: 'assistant', memories_deleted: 7, key_facts_deleted: 3, archive_deleted: 2, unattributed: 11, system_kept: 5, retained_shared: 13 }],
  not_covered: ['conversation text', 'tool_calls.jsonl', 'wiki', 'backups'],
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never });
});

describe('ForgetReceiptsPanel', () => {
  it('renders actual per-store counts separately from unattributed and retained system rows', async () => {
    mockWsClient.call.mockResolvedValue({ receipts: [receipt] });
    renderWithProviders(<ForgetReceiptsPanel />);
    expect(await screen.findByText(/Company Google/)).toBeInTheDocument();
    fireEvent.click(screen.getByText(/Company Google/));
    expect(screen.getByText('assistant')).toBeInTheDocument();
    for (const count of ['7', '3', '2', '11', '5', '13']) expect(screen.getByText(count)).toBeInTheDocument();
    expect(screen.getByText('Cannot attribute')).toBeInTheDocument();
    expect(screen.getByText('System rows kept')).toBeInTheDocument();
    expect(screen.getByText('Shared memories kept')).toBeInTheDocument();
    expect(screen.getByText('These memories stay because another connected source also supplied them.')).toBeInTheDocument();
    expect(screen.getByText(/Out of scope: conversation text, tool_calls.jsonl, wiki and backups/)).toBeInTheDocument();
  });

  it('does not present a failed list read as an empty completed history', async () => {
    mockWsClient.call.mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<ForgetReceiptsPanel />);
    expect(await screen.findByText('Could not load deletion receipts')).toBeInTheDocument();
    expect(screen.queryByText('No completed deletion receipts')).not.toBeInTheDocument();
  });

  it('downloads the receipt fetched by id, not the list snapshot', async () => {
    const completeReceipt = { ...receipt, approved_by: 'verified-admin' };
    mockWsClient.call.mockImplementation(async (method: string) => method === 'memory.forget_receipts_get'
      ? { receipt: completeReceipt } : { receipts: [receipt] });
    const create = vi.fn((_blob: Blob) => 'blob:receipt');
    const oldCreate = URL.createObjectURL;
    const oldRevoke = URL.revokeObjectURL;
    URL.createObjectURL = create;
    URL.revokeObjectURL = vi.fn();
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
    try {
      renderWithProviders(<ForgetReceiptsPanel />);
      fireEvent.click(await screen.findByText(/Company Google/));
      fireEvent.click(screen.getByRole('button', { name: 'Download JSON' }));
      await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('memory.forget_receipts_get', { id: 'receipt-1' }));
      await waitFor(() => expect(create).toHaveBeenCalledOnce());
      const blob = create.mock.calls[0][0] as Blob;
      const content = await new Promise<string>((resolve) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result));
        reader.readAsText(blob);
      });
      expect(JSON.parse(content)).toEqual(completeReceipt);
      expect(click).toHaveBeenCalledOnce();
    } finally {
      click.mockRestore();
      URL.createObjectURL = oldCreate;
      URL.revokeObjectURL = oldRevoke;
    }
  });

  it('does not fetch receipts before the gateway authenticates', () => {
    useConnectionStore.setState({ state: 'disconnected' as never });
    renderWithProviders(<ForgetReceiptsPanel />);
    expect(mockWsClient.call).not.toHaveBeenCalled();
    expect(screen.getByText('Connect to the gateway to load receipts.')).toBeInTheDocument();
  });

  // Added regression tests; not run in this change.
  it('shows quarantined failures and queues an explicit retry', async () => {
    const job = { id: 'batch-quarantined', display_name: 'Google retry', status: 'quarantined',
      attempt_count: 5, next_attempt_at: 0, last_error: 'missing batch plan' };
    mockWsClient.call.mockImplementation(async (method: string) => method === 'memory.forget_batch_retry'
      ? { status: 'queued' } : { receipts: [], jobs: [job] });
    renderWithProviders(<ForgetReceiptsPanel />);
    expect(await screen.findByText(/Google retry/)).toBeInTheDocument();
    expect(screen.getByText(/Failure reason: missing batch plan/)).toBeInTheDocument();
    expect(screen.getByText(/Consecutive failed or interrupted attempts: 5/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Retry deletion' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('memory.forget_batch_retry', { id: 'batch-quarantined' }));
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'memory.forget_receipts_list').length).toBeGreaterThan(1);
  });

  it('does not offer retry for a job waiting for approval', async () => {
    mockWsClient.call.mockResolvedValue({ receipts: [], jobs: [{ id: 'batch-pending', display_name: 'Google waiting',
      status: 'planned', attempt_count: 0, next_attempt_at: 0, last_error: null }] });
    renderWithProviders(<ForgetReceiptsPanel />);
    expect(await screen.findByText(/Google waiting/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry deletion' })).not.toBeInTheDocument();
  });

});
