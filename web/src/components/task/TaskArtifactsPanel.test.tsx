import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TaskArtifactsList, TaskArtifactsPanel } from './TaskArtifactsPanel';
import type { TaskArtifact } from '@/lib/api';

function artifact(over: Partial<TaskArtifact> = {}): TaskArtifact {
  return {
    name: 'summary.docx',
    archived_name: '1755000000000_summary.docx',
    agent_id: 'sales',
    origin: 'declared',
    attribution: 'exact',
    produced_at: '2026-08-15T10:00:00Z',
    size: 2048,
    round: null,
    channel: null,
    source_path: null,
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('<TaskArtifactsList> (pure, props-driven)', () => {
  it('renders one row per deliverable with its name and origin', () => {
    renderWithProviders(
      <TaskArtifactsList
        artifacts={[
          artifact({ name: 'a.docx' }),
          artifact({ name: 'b.xlsx', origin: 'swept' }),
        ]}
      />,
    );
    expect(screen.getByText('a.docx')).toBeInTheDocument();
    expect(screen.getByText('b.xlsx')).toBeInTheDocument();
    expect(screen.getByText('Handed over')).toBeInTheDocument();
    expect(screen.getByText('Auto-recovered')).toBeInTheDocument();
  });

  it('distinguishes an upload from something the AI staff member produced', () => {
    renderWithProviders(
      <TaskArtifactsList artifacts={[artifact({ origin: 'uploaded' })]} />,
    );
    expect(screen.getByText('You uploaded it')).toBeInTheDocument();
    expect(screen.queryByText('Handed over')).not.toBeInTheDocument();
  });

  it('labels a time-window match as an inference, never as a fact', () => {
    renderWithProviders(
      <TaskArtifactsList
        artifacts={[artifact({ attribution: 'inferred' })]}
        inferredCount={1}
      />,
    );
    expect(screen.getByText('Matched by time')).toBeInTheDocument();
    expect(screen.getByText(/matched by time only/)).toBeInTheDocument();
  });

  it('shows an honest empty state instead of claiming the task produced nothing', () => {
    renderWithProviders(<TaskArtifactsList artifacts={[]} />);
    expect(screen.getByText('This task has no recorded deliverable')).toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });

  it('builds download / preview links carrying the agent, archived name and token', () => {
    renderWithProviders(
      <TaskArtifactsList artifacts={[artifact({ name: 'report.pdf' })]} jwt="jwt-123" />,
    );
    const download = screen.getByLabelText('Download') as HTMLAnchorElement;
    expect(download.getAttribute('href')).toBe(
      '/api/files/download?agent=sales&name=1755000000000_summary.docx&token=jwt-123',
    );
    // A PDF previews inline through the same endpoint.
    const open = screen.getByLabelText('Open') as HTMLAnchorElement;
    expect(open.getAttribute('href')).toBe(download.getAttribute('href'));
  });

  it('routes office types through the server-side PDF preview', () => {
    renderWithProviders(<TaskArtifactsList artifacts={[artifact()]} jwt="j" />);
    expect(screen.getByLabelText('Open').getAttribute('href')).toContain('/api/files/preview');
  });

  it('offers no link for a file that was written but never archived', () => {
    renderWithProviders(
      <TaskArtifactsList
        artifacts={[
          artifact({
            name: 'draft.md',
            archived_name: null,
            origin: 'produced',
            size: null,
            round: 3,
            source_path: '/home/u/.duduclaw/agents/sales/draft.md',
          }),
        ]}
        jwt="j"
      />,
    );
    expect(screen.queryByLabelText('Download')).not.toBeInTheDocument();
    // It explains there is no copy instead of offering a link that would 404,
    // and never prints the internal path.
    expect(screen.getByText(/No archived copy of this file/)).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/\.duduclaw|\/home\//);
    expect(screen.getByText('Round 3')).toBeInTheDocument();
  });

  it('surfaces a load failure rather than rendering an empty deliverable list', () => {
    renderWithProviders(<TaskArtifactsList artifacts={[]} error="boom" />);
    expect(screen.getByText('Could not load the deliverables')).toBeInTheDocument();
    expect(screen.getByText('boom')).toBeInTheDocument();
  });
});

describe('<TaskArtifactsPanel> (fetching wrapper)', () => {
  it('asks tasks.artifacts for exactly the task it was given', async () => {
    mockWsClient.call.mockResolvedValue({
      artifacts: [artifact({ name: 'q3.xlsx' })],
      truncated: false,
      inferred_count: 0,
    });
    renderWithProviders(<TaskArtifactsPanel taskId="task-42" />);
    expect(await screen.findByText('q3.xlsx')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.artifacts', { task_id: 'task-42' });
  });

  it('reports an RPC failure to the user', async () => {
    mockWsClient.call.mockRejectedValue(new Error('rpc down'));
    renderWithProviders(<TaskArtifactsPanel taskId="task-42" />);
    expect(await screen.findByText('Could not load the deliverables. Try again later.')).toBeInTheDocument();
    expect(screen.queryByText('rpc down')).not.toBeInTheDocument();
  });
});

describe('P1 immutable artifact references', () => {
  it('keeps legacy artifacts unverified', () => {
    renderWithProviders(<TaskArtifactsList artifacts={[artifact()]} />);
    expect(screen.getByText('Unverified')).toBeInTheDocument();
    expect(screen.queryByText('Test evidence')).not.toBeInTheDocument();
  });
  it('shows a verified self-report beside stale bytes, with the captured hash', () => {
    const row = { ...artifact(), evidence: { task_revision: 1, content_hash: 'captured-hash', evidence_kind: 'self_report' as const, run_id: 'actual-run', audience: [], snapshot_verified: true }, integrity: 'stale' as const, stale_reasons: ['artifact_content_changed'] };
    renderWithProviders(<TaskArtifactsList artifacts={[row]} />);
    expect(screen.getByText('Stale')).toBeInTheDocument();
    expect(screen.getByText('Self-report')).toBeInTheDocument();
    expect(screen.getByText('The file changed after it was captured')).toBeInTheDocument();
    expect(screen.queryByText('artifact_content_changed')).not.toBeInTheDocument();
    expect(screen.getByText(/captured-hash/)).toBeInTheDocument();
    expect(screen.getByText(/actual-run/)).toBeInTheDocument();
    expect(screen.queryByText('Test evidence')).not.toBeInTheDocument();
  });
  it('does not show an operator-accepted badge for a ledger row the server did not verify', () => {
    const row = { ...artifact(), evidence: { task_revision: 1, content_hash: 'forged-hash', evidence_kind: 'operator' as const, run_id: 'forged-run', audience: [] }, integrity: 'current' as const };
    renderWithProviders(<TaskArtifactsList artifacts={[row]} />);
    expect(screen.queryByText('Operator accepted')).not.toBeInTheDocument();
    expect(screen.getByText('Record not verified against a snapshot')).toBeInTheDocument();
    expect(screen.queryByText(/forged-hash|forged-run/)).not.toBeInTheDocument();
  });
  it('shows the operator-accepted badge only when the server marks the row snapshot-verified', () => {
    const row = { ...artifact(), evidence: { task_revision: 1, content_hash: 'h', evidence_kind: 'operator' as const, run_id: null, audience: [], snapshot_verified: true }, integrity: 'current' as const };
    renderWithProviders(<TaskArtifactsList artifacts={[row]} />);
    expect(screen.getByText('Operator accepted')).toBeInTheDocument();
  });
  it('ignores a late artifact response for the previous task', async () => {
    let resolveOld!: (v: unknown) => void;
    mockWsClient.call.mockImplementationOnce(() => new Promise((r) => { resolveOld = r; }));
    mockWsClient.call.mockResolvedValueOnce({ artifacts: [artifact({ name: 'new-task.docx' })], truncated: false, inferred_count: 0 });
    const rendered = renderWithProviders(<TaskArtifactsPanel taskId="task-a" />);
    rendered.rerender(<TaskArtifactsPanel taskId="task-b" />);
    expect(await screen.findByText('new-task.docx')).toBeInTheDocument();
    resolveOld({ artifacts: [artifact({ name: 'old-task.docx' })], truncated: false, inferred_count: 0 });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText('old-task.docx')).not.toBeInTheDocument();
  });
});
