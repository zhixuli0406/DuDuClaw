import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within, act } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { TaskWorkflowReview, foldReview } from './TaskWorkflowReview';
import type { ReviewSnapshot } from './types';
import { workflowReviewApi, workflowDraftService, type ReviewResponse } from './workflow-api';
vi.mock('./workflow-api', () => ({ workflowReviewApi: { get: vi.fn(), capture: vi.fn(), accept: vi.fn() }, workflowDraftService: { list: vi.fn() } }));
beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'alice', email: 'alice@test.invalid', display_name: 'Alice', role: 'manager', status: 'active' } });
  vi.mocked(workflowReviewApi.get).mockResolvedValue({ snapshot: null });
  vi.mocked(workflowDraftService.list).mockResolvedValue([]);
});
function toggle(details: HTMLDetailsElement, open: boolean) { details.open = open; fireEvent(details, new Event('toggle')); }
describe('review request generations', () => {
  it('closing and reopening cancels busy state without accepting an old response', async () => {
    let resolve!: (r: ReviewResponse) => void;
    vi.mocked(workflowReviewApi.capture).mockImplementationOnce(() => new Promise((r) => { resolve = r; }));
    const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
    const details = rendered.container.querySelector('details')!; toggle(details, true);
    const button = await screen.findByRole('button', { name: 'Capture current review snapshot' });
    fireEvent.click(button); await waitFor(() => expect(button).toBeDisabled());
    toggle(details, false); toggle(details, true);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Capture current review snapshot' })).not.toBeDisabled());
    resolve({ snapshot: null });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Capture current review snapshot' })).not.toBeDisabled());
  });
  it('a task switch resets busy and prevents the former task capture from updating the new task', async () => {
    let reject!: (e: Error) => void;
    vi.mocked(workflowReviewApi.capture).mockImplementationOnce(() => new Promise((_, r) => { reject = r; }));
    const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
    toggle(rendered.container.querySelector('details')!, true);
    fireEvent.click(await screen.findByRole('button', { name: 'Capture current review snapshot' }));
    rendered.rerender(<TaskWorkflowReview taskId="task-b" completed />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Capture current review snapshot' })).not.toBeDisabled());
    reject(new Error('old task failure'));
    await waitFor(() => expect(workflowReviewApi.get).toHaveBeenCalledWith('task-b'));
    expect(screen.queryByText('old task failure')).not.toBeInTheDocument();
  });
});

function snap(id: string, hash: string): ReviewSnapshot {
  return { schema_version: 1, snapshot_id: id, snapshot_hash: hash, task_id: 'task-a', authority_revision: 1, authority_snapshot_hash: 'auth', criteria_ledger: null, captured_at: '2026-10-05', audience: [], gaps: [], waiting_for: null, next_check_at: null,
    artifacts: [{ artifact_id: 'a1', name: 'report.md', agent_id: 'sales', archived_name: null, source_path: null, source_hash: 'h', archived_hash: null, integrity: 'current', reasons: [], evidence_kind: 'self_report', run_id: null, audience: [] }] };
}
const review = (s: ReviewSnapshot): ReviewResponse => ({ snapshot: s, authority_current: true, acceptance: null });

describe('foldReview', () => {
  it('keeps the viewed snapshot and parks a different one as newer', () => {
    const a = review(snap('s1', 'h1')); const b = review(snap('s2', 'h2'));
    expect(foldReview(a, b)).toEqual({ viewed: a, newer: b });
    expect(foldReview(a, review(snap('s1', 'h1'))).newer).toBeNull();
    expect(foldReview(a, review(snap('s1', 'other-hash'))).newer).not.toBeNull();
    expect(foldReview(null, b)).toEqual({ viewed: b, newer: null });
  });
});

describe('snapshot pinning and confirmation', () => {
  it('does not swap the viewed snapshot on poll, and accepts exactly what was on screen after confirmation', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const first = review(snap('s1', 'hash-one-aaaaaaaaaa'));
      vi.mocked(workflowReviewApi.get).mockResolvedValue(first);
      vi.mocked(workflowReviewApi.accept).mockResolvedValue({});
      const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
      toggle(rendered.container.querySelector('details')!, true);
      const acceptButton = await screen.findByRole('button', { name: 'Accept this exact review snapshot' });
      // Someone else captures a newer snapshot; the next poll sees it.
      vi.mocked(workflowReviewApi.get).mockResolvedValue(review(snap('s2', 'hash-two-bbbbbbbbbb')));
      await act(async () => { await vi.advanceTimersByTimeAsync(5100); });
      expect(await screen.findByText(/A newer snapshot was captured/)).toBeInTheDocument();
      expect(screen.getByText(/hash-one-aaaaaaaaaa/)).toBeInTheDocument();
      expect(screen.queryByText(/hash-two-bbbbbbbbbb/)).not.toBeInTheDocument();
      // Accepting is paused until the person switches deliberately.
      expect(acceptButton).toBeDisabled();
      fireEvent.click(screen.getByRole('button', { name: 'View the newer snapshot' }));
      expect(await screen.findByText(/hash-two-bbbbbbbbbb/)).toBeInTheDocument();
      fireEvent.click(screen.getByRole('button', { name: 'Accept this exact review snapshot' }));
      const dialog = await screen.findByRole('dialog');
      expect(within(dialog).getByText(/starts with hash-two-bbb/)).toBeInTheDocument();
      expect(workflowReviewApi.accept).not.toHaveBeenCalled();
      fireEvent.click(within(dialog).getByRole('button', { name: 'Accept' }));
      await waitFor(() => expect(workflowReviewApi.accept).toHaveBeenCalledTimes(1));
      expect(vi.mocked(workflowReviewApi.accept).mock.calls[0][0]).toMatchObject({ snapshot_id: 's2', snapshot_hash: 'hash-two-bbbbbbbbbb' });
    } finally { vi.useRealTimers(); }
  });
  it('cancelling the accept confirmation accepts nothing', async () => {
    vi.mocked(workflowReviewApi.get).mockResolvedValue(review(snap('s1', 'hash-one-aaaaaaaaaa')));
    const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
    toggle(rendered.container.querySelector('details')!, true);
    fireEvent.click(await screen.findByRole('button', { name: 'Accept this exact review snapshot' }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(workflowReviewApi.accept).not.toHaveBeenCalled();
  });
  it('a poll answered after switching tasks never reaches the new task, and the old response is dropped', async () => {
    let resolveOld!: (r: ReviewResponse) => void;
    vi.mocked(workflowReviewApi.get).mockImplementationOnce(() => new Promise((r) => { resolveOld = r; }));
    vi.mocked(workflowReviewApi.get).mockResolvedValue(review(snap('s-new', 'hash-new-task')));
    const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
    toggle(rendered.container.querySelector('details')!, true);
    rendered.rerender(<TaskWorkflowReview taskId="task-b" completed />);
    await waitFor(() => expect(workflowReviewApi.get).toHaveBeenCalledWith('task-b'));
    await screen.findByText(/hash-new-task/);
    resolveOld(review(snap('s-old', 'hash-old-task')));
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText(/hash-old-task/)).not.toBeInTheDocument();
    expect(screen.queryByText(/A newer snapshot was captured/)).not.toBeInTheDocument();
  });
  it('marks the view as possibly out of date and hides the raw error when a refresh fails', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      vi.mocked(workflowReviewApi.get).mockResolvedValue(review(snap('s1', 'hash-one-aaaaaaaaaa')));
      const rendered = renderWithProviders(<TaskWorkflowReview taskId="task-a" completed />);
      toggle(rendered.container.querySelector('details')!, true);
      await screen.findByText(/hash-one-aaaaaaaaaa/);
      vi.mocked(workflowReviewApi.get).mockRejectedValue(new Error('rusqlite: database is locked'));
      await act(async () => { await vi.advanceTimersByTimeAsync(5100); });
      expect(await screen.findByText(/may be out of date/)).toBeInTheDocument();
      expect(document.body.textContent).not.toMatch(/rusqlite/);
      expect(screen.getByText(/hash-one-aaaaaaaaaa/)).toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });
});
