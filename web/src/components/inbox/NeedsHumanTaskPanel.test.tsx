import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { NeedsHumanTaskPanel, NeedsHumanActions } from './NeedsHumanTaskPanel';
import type { TaskInfo } from '@/lib/api';

const task = {
  id: 'task-1',
  title: 'Ship the release notes',
  description: 'Draft and publish the notes',
  status: 'needs_human',
  priority: 'medium',
  assigned_to: 'writer',
  created_by: 'goal:dashboard',
  created_at: '2026-08-15T10:00:00Z',
  updated_at: '2026-08-15T10:30:00Z',
  judge_feedback: 'Acceptance criteria 2 is unmet',
} as unknown as TaskInfo;

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('<NeedsHumanTaskPanel> WP-F changes tab', () => {
  it('shows the brief (escalation reason) by default and does not fetch changes', () => {
    renderWithProviders(
      <NeedsHumanTaskPanel task={task} typeLabel="Awaiting you" onResolved={() => {}} />,
    );
    expect(screen.getByText('Acceptance criteria 2 is unmet')).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('tasks.changes', expect.anything());
  });

  it('loads and renders the recorded file changes once the 變更 tab is opened', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'tasks.changes') {
        return Promise.resolve({
          changes: [
            {
              path: '/repo/RELEASE.md',
              op: 'write',
              tool_name: 'Write',
              timestamp: '2026-08-15T10:20:00Z',
              success: true,
              snippet: '# v1.60',
              source: 'native',
              round: 1,
            },
          ],
          distinct_paths: 1,
          truncated: false,
        });
      }
      return Promise.resolve({});
    });

    renderWithProviders(
      <NeedsHumanTaskPanel task={task} typeLabel="Awaiting you" onResolved={() => {}} />,
    );
    await userEvent.click(screen.getByRole('tab', { name: /Changes/ }));

    expect(await screen.findByText('/repo/RELEASE.md')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.changes', { task_id: 'task-1' });
  });
});

// WP-10B: the shared `NeedsHumanActions` component (Inbox detail pane,
// `/tasks/:id`, the task board card) previously had no way to attach a note
// to 重試, even though `tasks.goal_decide` has accepted `note` since the
// `/goals` board's `InterventionButtons` shipped it (I-3c) — this closed
// that gap by reusing the same toggle-then-submit affordance here.
describe('<NeedsHumanActions> optional retry note (WP-10B)', () => {
  it('retry opens a note field and submits it alongside the retry decision', async () => {
    const onResolved = vi.fn();
    renderWithProviders(<NeedsHumanActions taskId="task-1" onResolved={onResolved} />);

    // First click only reveals the note field — it must NOT fire the retry
    // decision immediately (that would strand a half-typed note).
    await userEvent.click(screen.getByRole('button', { name: /Retry/ }));
    expect(mockWsClient.call).not.toHaveBeenCalled();

    const noteField = screen.getByPlaceholderText('Instruction for the next round (optional)');
    await userEvent.type(noteField, 'please check the API key first');
    await userEvent.click(screen.getByRole('button', { name: 'Retry now' }));

    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
      task_id: 'task-1',
      action: 'retry',
      note: 'please check the API key first',
    });
    expect(onResolved).toHaveBeenCalled();
  });

  it('retry with an empty note behaves identically to the pre-WP-10B path (note: "")', async () => {
    renderWithProviders(<NeedsHumanActions taskId="task-1" onResolved={() => {}} />);

    await userEvent.click(screen.getByRole('button', { name: /Retry/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Retry now' }));

    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
      task_id: 'task-1',
      action: 'retry',
      note: '',
    });
  });

  it('標記完成 / 放棄 stay immediate — no note field, one click, same as before', async () => {
    const onResolved = vi.fn();
    renderWithProviders(<NeedsHumanActions taskId="task-1" onResolved={onResolved} />);

    await userEvent.click(screen.getByRole('button', { name: /Mark complete/ }));

    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
      task_id: 'task-1',
      action: 'done',
      note: '',
    });
    expect(screen.queryByPlaceholderText('Instruction for the next round (optional)')).not.toBeInTheDocument();
    expect(onResolved).toHaveBeenCalled();
  });
});
