import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import en from '@/i18n/en.json';
import { renderWithProviders } from '@/test/render';
import type { AgentFileReview, AgentFileSaveResponse, AgentFileSaveStatus } from '@/lib/agent-files-types';
import { ChangeReviewDialog, type ChangeReviewState } from './ChangeReviewDialog';
import { responseNeedsDialog } from './review-text';

function review(over: Partial<AgentFileReview> = {}): AgentFileReview {
  return {
    analysis_id: 'an-1',
    verdict: 'warn',
    basis: 'rules+ai',
    summary: '',
    reasons: [
      { category: 'loosened_redline', severity: 'medium', message: '放寬了禁止事項', evidence: 'may share prices' },
    ],
    can_confirm: true,
    can_request_approval: true,
    ...over,
  };
}

function res(status: AgentFileSaveStatus, over: Partial<AgentFileSaveResponse> = {}): ChangeReviewState {
  return {
    kind: 'response',
    response: { status, file: 'contract', agent_id: 'my-bot', ...over },
  };
}

function renderDialog(state: ChangeReviewState | null, isAdmin = false) {
  const handlers = {
    onConfirm: vi.fn(),
    onRequestApproval: vi.fn(),
    onClose: vi.fn(),
    onReload: vi.fn(),
    onApplied: vi.fn(),
  };
  renderWithProviders(<ChangeReviewDialog state={state} isAdmin={isAdmin} {...handlers} />);
  return handlers;
}

const confirmName = en['agentFiles.review.confirm'];
const requestName = en['agentFiles.review.requestApproval'];

describe('<ChangeReviewDialog>', () => {
  it('stays closed without a state', () => {
    renderDialog(null);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('shows the checking state with a spinner and no buttons', () => {
    renderDialog({ kind: 'checking' });
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveAccessibleName(en['agentFiles.review.title.checking']);
    expect(screen.getByText(en['agentFiles.review.checking'])).toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });

  it('closes itself on applied and tells the caller once', () => {
    const state = res('applied', { hash: 'h2', review: review({ verdict: 'pass', reasons: [] }) });
    const h = renderDialog(state);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(h.onApplied).toHaveBeenCalledTimes(1);
  });

  it('keeps applied open when the automatic check did not finish', () => {
    renderDialog(res('applied', { review: review({ verdict: 'pass', basis: 'rules+ai_unavailable', reasons: [] }) }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(screen.getByText(en['agentFiles.review.analysisUnavailable'])).toBeInTheDocument();
  });

  it('needs_confirmation: plain reasons, evidence, and a required reason', async () => {
    const user = userEvent.setup();
    const h = renderDialog(res('needs_confirmation', { review: review() }));
    expect(screen.getByRole('dialog')).toHaveAccessibleName(en['agentFiles.review.title.needsConfirmation']);
    expect(screen.getByText(en['agentFiles.category.loosened_redline'])).toBeInTheDocument();
    expect(screen.getByText('may share prices')).toBeInTheDocument();

    const confirm = screen.getByRole('button', { name: confirmName });
    const request = screen.getByRole('button', { name: requestName });
    expect(confirm).toBeDisabled();
    expect(request).toBeDisabled();

    const box = screen.getByLabelText(en['agentFiles.review.reasonLabel']);
    await user.type(box, 'abc');
    expect(confirm).toBeDisabled();
    await user.type(box, 'd');
    expect(confirm).toBeEnabled();
    expect(request).toBeEnabled();

    await user.click(confirm);
    expect(h.onConfirm).toHaveBeenCalledWith('abcd', false);
    await user.click(request);
    expect(h.onRequestApproval).toHaveBeenCalledWith('abcd');
  });

  it('needs_confirmation for an admin offers no review request', () => {
    renderDialog(res('needs_confirmation', { review: review() }), true);
    expect(screen.getByRole('button', { name: confirmName })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: requestName })).not.toBeInTheDocument();
  });

  it('blocked for a non-admin: only a review request, never a confirm button', async () => {
    const user = userEvent.setup();
    const h = renderDialog(
      res('blocked', { review: review({ verdict: 'block', can_confirm: false, reasons: [
        { category: 'prompt_injection', severity: 'high', message: 'x', evidence: '' },
      ] }) }),
    );
    expect(screen.getByRole('dialog')).toHaveAccessibleName(en['agentFiles.review.title.blocked']);
    expect(screen.getByText(en['agentFiles.category.prompt_injection'])).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: confirmName })).not.toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    await user.type(screen.getByLabelText(en['agentFiles.review.reasonLabel']), 'please check');
    await user.click(screen.getByRole('button', { name: requestName }));
    expect(h.onRequestApproval).toHaveBeenCalledWith('please check');
  });

  it('blocked for a non-admin hides confirm even if the server flag says it may', () => {
    renderDialog(res('blocked', { review: review({ verdict: 'block', can_confirm: true }) }));
    expect(screen.queryByRole('button', { name: confirmName })).not.toBeInTheDocument();
  });

  it('blocked for an admin needs a reason and the risk checkbox', async () => {
    const user = userEvent.setup();
    const h = renderDialog(res('blocked', { review: review({ verdict: 'block', can_confirm: true, can_request_approval: false }) }), true);
    const confirm = screen.getByRole('button', { name: confirmName });
    expect(screen.queryByRole('button', { name: requestName })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText(en['agentFiles.review.reasonLabel']), 'known safe');
    expect(confirm).toBeDisabled();
    await user.click(screen.getByRole('checkbox', { name: en['agentFiles.review.acknowledge'] }));
    expect(confirm).toBeEnabled();
    await user.click(confirm);
    expect(h.onConfirm).toHaveBeenCalledWith('known safe', true);
  });

  it('says plainly when the automatic check did not finish', () => {
    renderDialog(res('needs_confirmation', { review: review({
      basis: 'rules+ai_unavailable',
      reasons: [{ category: 'analysis_unavailable', severity: 'medium', message: '', evidence: '' }],
    }) }));
    expect(screen.getByText(en['agentFiles.review.analysisUnavailable'])).toBeInTheDocument();
    // The notice replaces the bare category line, so it is not repeated.
    expect(screen.queryByText(en['agentFiles.category.analysis_unavailable'])).not.toBeInTheDocument();
  });

  it('rejected: only the list of things to fix and a close button', async () => {
    const user = userEvent.setup();
    const h = renderDialog(res('rejected', { review: review({ verdict: 'block', can_confirm: false, can_request_approval: false, reasons: [
      { category: 'format', severity: 'high', message: 'bad', evidence: 'line 3' },
      { category: 'size', severity: 'high', message: 'big', evidence: '' },
    ] }) }));
    expect(screen.getByText(en['agentFiles.category.format'])).toBeInTheDocument();
    expect(screen.getByText(en['agentFiles.category.size'])).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: confirmName })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: requestName })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(en['agentFiles.review.reasonLabel'])).not.toBeInTheDocument();
    expect(screen.getAllByRole('button')).toHaveLength(1);
    await user.click(screen.getByRole('button', { name: en['agentFiles.review.close'] }));
    expect(h.onClose).toHaveBeenCalled();
  });

  it('conflict: explains and offers a reload', async () => {
    const user = userEvent.setup();
    const h = renderDialog(res('conflict', { current_hash: 'h9' }));
    expect(screen.getByText(en['agentFiles.review.conflict'])).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: en['agentFiles.review.reload'] }));
    expect(h.onReload).toHaveBeenCalled();
  });

  it('pending_approval: shows the short request number', () => {
    renderDialog(res('pending_approval', { approval_id: 'abcdef12-3456-7890' }));
    expect(screen.getByRole('dialog')).toHaveAccessibleName(en['agentFiles.review.title.pending']);
    expect(screen.getByText(/abcdef12/)).toBeInTheDocument();
    expect(screen.queryByText(/3456-7890/)).not.toBeInTheDocument();
  });

  it('unchanged never opens', () => {
    renderDialog(res('unchanged'));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('closes on Escape', async () => {
    const user = userEvent.setup();
    const h = renderDialog(res('conflict'));
    await user.keyboard('{Escape}');
    expect(h.onClose).toHaveBeenCalled();
  });
});

describe('responseNeedsDialog', () => {
  it('opens for everything except a clean save', () => {
    expect(responseNeedsDialog({ status: 'applied' })).toBe(false);
    expect(responseNeedsDialog({ status: 'unchanged' })).toBe(false);
    expect(responseNeedsDialog(undefined)).toBe(false);
    expect(responseNeedsDialog({ status: 'applied', review: review({ basis: 'rules+ai_unavailable' }) })).toBe(true);
    for (const s of ['needs_confirmation', 'blocked', 'pending_approval', 'conflict', 'rejected'] as const) {
      expect(responseNeedsDialog({ status: s })).toBe(true);
    }
  });
});
