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

  it('labels a discovery approval instead of falling back to "other" (L7)', () => {
    renderWithProviders(<ApprovalRequestCard payload={{ ...ITEM, kind: 'discovery' }} />);
    expect(screen.getByText('Code discovery')).toBeInTheDocument();
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

  describe('knowledge conflict held for review', () => {
    const HELD: ApprovalItem = {
      ...ITEM,
      kind: 'knowledge_quarantine',
      summary: '對話中出現一則與現有記憶「Acme」衝突的說法…內容摘要：Acme moved to Taipei',
      payload: { subject: 'Acme', quarantined_ids: ['m1'], promote_on_approve: true },
    };

    it('approving reports that the memory was updated', async () => {
      vi.spyOn(api.approvals, 'decide').mockResolvedValue({
        id: 'appr-1',
        decided: 'approved',
        side_effect: { quarantine_promoted: 1 },
      });
      renderWithProviders(<ApprovalRequestCard payload={HELD} />);
      await userEvent.setup().click(screen.getByRole('button', { name: 'Approve' }));
      expect(
        await screen.findByText('Memory updated: the new statement replaced the old one'),
      ).toBeInTheDocument();
    });

    it('rejecting reports that the current memory stays', async () => {
      vi.spyOn(api.approvals, 'decide').mockResolvedValue({
        id: 'appr-1',
        decided: 'denied',
        side_effect: { quarantine_rejected: 1 },
      });
      renderWithProviders(<ApprovalRequestCard payload={HELD} />);
      await userEvent.setup().click(screen.getByRole('button', { name: 'Reject' }));
      expect(
        await screen.findByText('The new statement was discarded and the current memory stays'),
      ).toBeInTheDocument();
    });

    it('a stale decision says nothing was written', async () => {
      vi.spyOn(api.approvals, 'decide').mockResolvedValue({
        id: 'appr-1',
        decided: 'approved',
        side_effect: { quarantine_stale: 1 },
      });
      renderWithProviders(<ApprovalRequestCard payload={HELD} />);
      await userEvent.setup().click(screen.getByRole('button', { name: 'Approve' }));
      expect(await screen.findByText(/nothing was written/)).toBeInTheDocument();
    });

    it('approving a batch reports released and held-back counts', async () => {
      vi.spyOn(api.approvals, 'decide').mockResolvedValue({
        id: 'appr-1',
        decided: 'approved',
        side_effect: { quarantine_released: 2, quarantine_held: 1 },
      });
      renderWithProviders(
        <ApprovalRequestCard payload={{ ...HELD, payload: { subject: 'x', quarantined_ids: ['a', 'b', 'c'] } }} />,
      );
      await userEvent.setup().click(screen.getByRole('button', { name: 'Approve' }));
      expect(
        await screen.findByText(
          '2 accepted; 1 conflicted with more reliable content and were set aside as separate items to confirm',
        ),
      ).toBeInTheDocument();
    });

    it('says the full text is in the inbox', () => {
      renderWithProviders(<ApprovalRequestCard payload={HELD} />);
      expect(
        screen.getByText('The full new statement and the current content are in the inbox.'),
      ).toBeInTheDocument();
    });

    it('labels the kind instead of falling back to "other"', () => {
      renderWithProviders(<ApprovalRequestCard payload={HELD} />);
      expect(screen.getByText('Knowledge to confirm')).toBeInTheDocument();
    });
  });
});
