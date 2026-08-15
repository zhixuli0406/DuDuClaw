import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { NeedsHumanTaskPanel } from './NeedsHumanTaskPanel';
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
