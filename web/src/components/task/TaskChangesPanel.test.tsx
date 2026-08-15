import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TaskChangesList, TaskChangesPanel } from './TaskChangesPanel';
import type { TaskChange } from '@/lib/api';

function change(over: Partial<TaskChange> = {}): TaskChange {
  return {
    path: '/repo/src/main.rs',
    op: 'write',
    tool_name: 'Write',
    timestamp: '2026-08-15T10:00:00Z',
    success: true,
    snippet: null,
    source: 'native',
    round: null,
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('<TaskChangesList> (pure, props-driven)', () => {
  it('renders one row per change with its path, op label and tool', () => {
    renderWithProviders(
      <TaskChangesList
        changes={[
          change({ path: '/repo/a.rs', op: 'write' }),
          change({ path: '/repo/b.rs', op: 'edit', tool_name: 'Edit' }),
        ]}
        distinctPaths={2}
      />,
    );
    expect(screen.getByText('/repo/a.rs')).toBeInTheDocument();
    expect(screen.getByText('/repo/b.rs')).toBeInTheDocument();
    expect(screen.getByText('Create / overwrite')).toBeInTheDocument();
    // "Edit" is both the op label and the tool name on the second row.
    expect(screen.getAllByText('Edit')).toHaveLength(2);
  });

  it('shows an honest empty state instead of implying nothing changed', () => {
    renderWithProviders(<TaskChangesList changes={[]} />);
    expect(screen.getByText('This task left no file-change record')).toBeInTheDocument();
    // No fabricated summary line.
    expect(screen.queryByText(/file\(s\) touched/)).not.toBeInTheDocument();
  });

  it('marks failed calls — the ones live tool state hides', () => {
    renderWithProviders(<TaskChangesList changes={[change({ success: false })]} distinctPaths={1} />);
    expect(screen.getByText('Failed')).toBeInTheDocument();
  });

  it('renders the masked snippet verbatim', () => {
    renderWithProviders(
      <TaskChangesList changes={[change({ snippet: 'API_KEY=***MASKED***' })]} distinctPaths={1} />,
    );
    expect(screen.getByText('API_KEY=***MASKED***')).toBeInTheDocument();
  });

  it('renders a shell row with its command and the command copy affordance', () => {
    renderWithProviders(
      <TaskChangesList
        changes={[change({ op: 'shell', tool_name: 'Bash', path: 'rm -rf /repo/build' })]}
      />,
    );
    expect(screen.getByText('rm -rf /repo/build')).toBeInTheDocument();
    expect(screen.getByLabelText('Copy command')).toBeInTheDocument();
  });

  it('offers a copy-path button per file row', () => {
    renderWithProviders(<TaskChangesList changes={[change()]} distinctPaths={1} />);
    expect(screen.getByLabelText('Copy path')).toBeInTheDocument();
  });

  it('says so when the result was capped', () => {
    renderWithProviders(<TaskChangesList changes={[change()]} distinctPaths={1} truncated />);
    expect(screen.getByText('Showing the most recent records only.')).toBeInTheDocument();
  });

  it('surfaces a load error rather than an empty state', () => {
    renderWithProviders(<TaskChangesList changes={[]} error="boom" />);
    expect(screen.getByText('Could not load the change record')).toBeInTheDocument();
    expect(screen.getByText('boom')).toBeInTheDocument();
  });
});

describe('<TaskChangesPanel> (fetching wrapper)', () => {
  it('calls tasks.changes for the task and renders the returned rows', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'tasks.changes') {
        return Promise.resolve({
          changes: [change({ path: '/repo/only.rs', round: 2 })],
          distinct_paths: 1,
          truncated: false,
        });
      }
      return Promise.resolve({});
    });
    renderWithProviders(<TaskChangesPanel taskId="task-1" />);
    expect(await screen.findByText('/repo/only.rs')).toBeInTheDocument();
    expect(screen.getByText('Round 2')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.changes', { task_id: 'task-1' });
  });

  it('falls back to the honest empty state when the backend returns no evidence', async () => {
    mockWsClient.call.mockResolvedValue({ changes: [], distinct_paths: 0, truncated: false });
    renderWithProviders(<TaskChangesPanel taskId="task-1" />);
    expect(await screen.findByText('This task left no file-change record')).toBeInTheDocument();
  });
});
