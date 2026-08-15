import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { TaskFilesList, TaskFilesPanel, type TaskFileRow } from './TaskFilesPanel';

function fileRow(over: Partial<TaskFileRow> = {}): TaskFileRow {
  return {
    name: 'summary.docx',
    size: 2048,
    mtime: Date.parse('2026-08-15T10:00:00Z'),
    origin: 'declared',
    task_id: 'task-42',
    round: null as unknown as number | undefined,
    display_name: undefined,
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('<TaskFilesList> (pure, props-driven)', () => {
  it('groups rows into 收到的 vs 做出來的 by origin', () => {
    renderWithProviders(
      <TaskFilesList
        agentId="nova"
        files={[
          fileRow({ name: 'upload.pdf', origin: 'uploaded' }),
          fileRow({ name: 'report.docx', origin: 'declared' }),
        ]}
      />,
    );
    expect(screen.getByText('Received')).toBeInTheDocument();
    expect(screen.getByText('Produced')).toBeInTheDocument();
    expect(screen.getByText('upload.pdf')).toBeInTheDocument();
    expect(screen.getByText('report.docx')).toBeInTheDocument();
  });

  it('buckets an unknown-origin file separately instead of guessing produced vs received', () => {
    renderWithProviders(
      <TaskFilesList agentId="nova" files={[fileRow({ name: 'mystery.txt', origin: 'unknown' })]} />,
    );
    // Appears twice: the group heading and the row's own origin badge.
    expect(screen.getAllByText('Source unknown')).toHaveLength(2);
    expect(screen.queryByText('Received')).not.toBeInTheDocument();
    expect(screen.queryByText('Produced')).not.toBeInTheDocument();
  });

  it('shows an honest empty state when the task has files evidence but zero rows', () => {
    renderWithProviders(<TaskFilesList agentId="nova" files={[]} />);
    expect(screen.getByText('No files for this task')).toBeInTheDocument();
  });

  it('shows a distinct honest state when no agent is assigned, instead of guessing a bucket', () => {
    renderWithProviders(<TaskFilesList agentId={null} files={[]} />);
    expect(screen.getByText("This task has no assigned AI staff member, so its files can't be located")).toBeInTheDocument();
  });

  it('surfaces a load failure rather than an empty list', () => {
    renderWithProviders(<TaskFilesList agentId="nova" files={[]} error="boom" />);
    expect(screen.getByText('Failed to load files')).toBeInTheDocument();
    expect(screen.getByText('boom')).toBeInTheDocument();
  });

  it('builds download links carrying the agent, file name and token', () => {
    renderWithProviders(
      <TaskFilesList agentId="nova" files={[fileRow({ name: 'report.pdf' })]} jwt="jwt-123" />,
    );
    const download = screen.getByLabelText('Download') as HTMLAnchorElement;
    expect(download.getAttribute('href')).toBe('/api/files/download?agent=nova&name=report.pdf&token=jwt-123');
  });
});

describe('<TaskFilesPanel> (fetching wrapper)', () => {
  it('fetches /api/files scoped to the agent and filters client-side to this task', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        files: [
          { name: 'ours.docx', size: 10, mtime: 1, origin: 'declared', task_id: 'task-42' },
          { name: 'other-task.docx', size: 10, mtime: 1, origin: 'declared', task_id: 'task-99' },
        ],
      }),
    });
    vi.stubGlobal('fetch', fetchMock);

    renderWithProviders(<TaskFilesPanel taskId="task-42" agentId="nova" />);

    expect(await screen.findByText('ours.docx')).toBeInTheDocument();
    expect(screen.queryByText('other-task.docx')).not.toBeInTheDocument();
    expect(String(fetchMock.mock.calls[0][0])).toContain('/api/files?agent=nova');
  });

  it('reports a fetch failure to the user', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 500, json: async () => ({}) }));
    renderWithProviders(<TaskFilesPanel taskId="task-42" agentId="nova" />);
    expect(await screen.findByText('HTTP 500')).toBeInTheDocument();
  });

  it('never calls fetch when no agent is assigned', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    renderWithProviders(<TaskFilesPanel taskId="task-42" agentId={null} />);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
