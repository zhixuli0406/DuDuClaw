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
    // It says where the file is instead of offering a link that would 404.
    expect(screen.getByText(/no archived copy to download/)).toBeInTheDocument();
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
    expect(await screen.findByText('rpc down')).toBeInTheDocument();
  });
});
