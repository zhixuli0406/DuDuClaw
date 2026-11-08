import { mockWsClient } from '@/test/mocks';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { ApprovalItem } from '@/lib/api';
import { ApprovalDetailPanel } from './ApprovalDetailPanel';

function orgApproval(): ApprovalItem {
  return {
    id: 'apr-org-1',
    agent_id: 'quote',
    kind: 'org_change',
    summary: 'Move Quote bot to report to Sam',
    created_at: new Date().toISOString(),
    ttl_seconds: 86400,
    payload: {
      agent_id: 'quote',
      before: { reports_to: 'dudu', department: 'Sales' },
      after: { reports_to: 'sales', department: 'Sales' },
      base: 'h1',
      proposer: 'u-lin',
      proposer_email: 'lin@test.invalid',
      reason: 'Sales handles quotes now',
    },
  } as ApprovalItem;
}

function pendingRow(stale: boolean) {
  return {
    approval_id: 'apr-org-1', agent_id: 'quote', before: { reports_to: 'dudu', department: 'Sales' },
    after: { reports_to: 'sales', department: 'Sales' }, base: 'h1', stale, proposer: 'u-lin',
    proposer_email: 'lin@test.invalid', reason: 'Sales handles quotes now', created_at: '2026-11-08T00:00:00Z', expires_at: null,
  };
}

function renderCard(stale: boolean | 'unavailable') {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method !== 'org.pending_changes') return {};
    if (stale === 'unavailable') throw new Error('nope');
    return { count: 1, pending: [pendingRow(stale)] };
  });
  const onApprove = vi.fn();
  const onReject = vi.fn();
  renderWithProviders(
    <ApprovalDetailPanel approval={orgApproval()} agentName="Quote bot" onApprove={onApprove} onReject={onReject} />,
  );
  return { onApprove, onReject };
}

afterEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue(null);
});

describe('org_change approval card', () => {
  it('shows the employee, the before/after placement, who asked and why, with Approve and Reject', async () => {
    const { onApprove, onReject } = renderCard(false);
    const card = await screen.findByTestId('org-change');
    expect(within(card).getByText('Org change request')).toBeInTheDocument();
    expect(within(card).getByText('Quote bot')).toBeInTheDocument();
    expect(within(card).getByTestId('org-change-manager-before')).toHaveTextContent('dudu');
    expect(within(card).getByTestId('org-change-manager-after')).toHaveTextContent('sales');
    expect(within(card).getByText('(unchanged)')).toBeInTheDocument();
    expect(within(card).getByText('lin@test.invalid')).toBeInTheDocument();
    expect(within(card).getByText('Sales handles quotes now')).toBeInTheDocument();
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('org.pending_changes', { agent_id: 'quote' }));
    expect(within(card).queryByRole('alert')).not.toBeInTheDocument();

    const approve = screen.getByRole('button', { name: 'Approve' });
    expect(approve).toBeEnabled();
    fireEvent.click(approve);
    expect(onApprove).toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    expect(onReject).toHaveBeenCalled();
  });

  it('a stale request says so and can only be rejected', async () => {
    const { onApprove, onReject } = renderCard(true);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The organization has changed since this request. Please submit a new one.');
    await waitFor(() => expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
    expect(onApprove).not.toHaveBeenCalled();
    const reject = screen.getByRole('button', { name: 'Reject' });
    expect(reject).toBeEnabled();
    fireEvent.click(reject);
    expect(onReject).toHaveBeenCalled();
  });

  it('still renders from the request itself when the waiting list cannot be read', async () => {
    renderCard('unavailable');
    const card = await screen.findByTestId('org-change');
    expect(within(card).getByTestId('org-change-manager-after')).toHaveTextContent('sales');
    expect(screen.getByRole('button', { name: 'Approve' })).toBeEnabled();
  });
});
