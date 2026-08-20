import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { ApprovalRequestCard } from './ApprovalRequestCard';
import { api, type ApprovalItem } from '@/lib/api';

const ITEM: ApprovalItem = {
  id: 'appr-1',
  agent_id: 'ops-agent',
  kind: 'tool_call',
  summary: 'Install the pending system update',
  payload: {},
  created_at: new Date().toISOString(),
  ttl_seconds: 300,
};

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('<ApprovalRequestCard> — O-3 inline HITL approval', () => {
  it('renders the request summary, requester, and kind', () => {
    renderWithProviders(<ApprovalRequestCard payload={ITEM} />);
    expect(screen.getByText('Install the pending system update')).toBeInTheDocument();
    expect(screen.getByText(/ops-agent/)).toBeInTheDocument();
    expect(screen.getByText('Tool call')).toBeInTheDocument();
  });

  it('approve calls approvals.decide(id, true) and resolves the card', async () => {
    const decide = vi.spyOn(api.approvals, 'decide').mockResolvedValue({ id: 'appr-1', decided: 'approved' });
    renderWithProviders(<ApprovalRequestCard payload={ITEM} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole('button', { name: 'Approve' }));

    await waitFor(() => expect(decide).toHaveBeenCalledWith('appr-1', true));
    expect(await screen.findByText('Approved: Install the pending system update')).toBeInTheDocument();
    // Resolved — the approve/deny buttons are gone, this can't be double-fired.
    expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  });

  it('reject calls approvals.decide(id, false) and resolves the card', async () => {
    const decide = vi.spyOn(api.approvals, 'decide').mockResolvedValue({ id: 'appr-1', decided: 'denied' });
    renderWithProviders(<ApprovalRequestCard payload={ITEM} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole('button', { name: 'Reject' }));

    await waitFor(() => expect(decide).toHaveBeenCalledWith('appr-1', false));
    expect(await screen.findByText('Rejected: Install the pending system update')).toBeInTheDocument();
  });

  it('a failed decide keeps the card pending and surfaces the error for retry', async () => {
    const decide = vi
      .spyOn(api.approvals, 'decide')
      .mockRejectedValueOnce(new Error('network unreachable'))
      .mockResolvedValueOnce({ id: 'appr-1', decided: 'approved' });
    renderWithProviders(<ApprovalRequestCard payload={ITEM} />);
    const user = userEvent.setup();

    const approveButton = screen.getByRole('button', { name: 'Approve' });
    await user.click(approveButton);

    expect(await screen.findByRole('alert')).toHaveTextContent('network unreachable');
    expect(approveButton).toBeEnabled();

    await user.click(approveButton);
    await waitFor(() => expect(decide).toHaveBeenCalledTimes(2));
  });

  it('renders the ActionGuard simulation narrative when present', () => {
    renderWithProviders(
      <ApprovalRequestCard
        payload={{ ...ITEM, simulation: { world_state_change: 'The device restarts.', risk_points: [] } }}
      />,
    );
    expect(screen.getByText('The device restarts.')).toBeInTheDocument();
  });
});
