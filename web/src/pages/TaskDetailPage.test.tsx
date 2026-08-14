import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { TaskDetailPage } from './TaskDetailPage';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { TaskInfo } from '@/lib/api';

const AGENTS = [{ name: 'nova', display_name: 'Nova', status: 'active', role: 'main', sandboxed: false }];

const TASK: TaskInfo = {
  id: 'task-aaaa1111',
  title: 'Draft the launch plan',
  description: 'The rollout checklist',
  status: 'todo',
  priority: 'high',
  assigned_to: 'nova',
  created_by: 'user',
  created_at: '2026-07-17T00:00:00Z',
  updated_at: '2026-07-17T00:00:00Z',
  tags: [],
};

function renderAt(id: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[`/tasks/${id}`]}>
        <Routes>
          <Route path="/tasks/:id" element={<TaskDetailPage />} />
          <Route path="/tasks" element={<div>board-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ tasks: [TASK], agents: AGENTS, events: [], comments: [] });
  useTasksStore.setState({ tasks: [TASK], comments: {}, activities: [], loading: false });
  useAgentsStore.setState({ agents: AGENTS as never[], loading: false });
});

describe('TaskDetailPage', () => {
  it('renders the breadcrumb header and inline-editable title', () => {
    renderAt('task-aaaa1111');
    // Breadcrumb root segment back to the board.
    expect(screen.getByText('Task Board')).toBeInTheDocument();
    // Title renders as the inline-editor resting button.
    expect(screen.getByRole('button', { name: 'Task title' })).toHaveTextContent('Draft the launch plan');
  });

  it('exposes the mark-complete and detail-toggle header actions', () => {
    renderAt('task-aaaa1111');
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Toggle details' })).toBeInTheDocument();
  });

  it('shows the not-found state for an unknown id', async () => {
    renderAt('does-not-exist');
    await waitFor(() => {
      expect(screen.getByText('Task not found')).toBeInTheDocument();
    });
  });

  it('shows the Live pill while an active agent runs an in-progress task', async () => {
    const inProgress = { ...TASK, status: 'in_progress' as const };
    mockWsClient.call.mockResolvedValue({ tasks: [inProgress], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [inProgress], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    await waitFor(() => expect(screen.getByText('Live')).toBeInTheDocument());
  });

  it('hides the Live pill once the task is done, so it never reads a stale status (#4)', async () => {
    const done = { ...TASK, status: 'done' as const };
    mockWsClient.call.mockResolvedValue({ tasks: [done], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [done], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Task title' })).toBeInTheDocument(),
    );
    expect(screen.queryByText('Live')).toBeNull();
  });
});

// ── WP-A (§2-6): the detail page used to hand a 等你決定 task to the generic
// status picker, which could not express "retry" at all (that writes `pending`,
// a status the picker never listed). It now shows the same three choices as the
// inbox, and nothing else can write the task's status.

const NEEDS_HUMAN: TaskInfo = {
  ...TASK,
  status: 'needs_human',
  judge_feedback: 'Could not confirm the refund amount',
};

describe('TaskDetailPage — 等你決定 (WP-A §2-6)', () => {
  beforeEach(() => {
    mockWsClient.call.mockResolvedValue({ tasks: [NEEDS_HUMAN], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [NEEDS_HUMAN], comments: {}, activities: [], loading: false });
  });

  it('shows the decision, its reason, and the same three choices as the inbox', () => {
    renderAt('task-aaaa1111');
    expect(
      screen.getByText('This task is waiting on your decision: try again, call it done, or give up?'),
    ).toBeInTheDocument();
    expect(screen.getByText('Could not confirm the refund amount')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Give up' })).toBeInTheDocument();
  });

  it('hides the header quick-complete so the decision panel is the only writer', () => {
    renderAt('task-aaaa1111');
    // The header action carries the aria-label "Mark complete" too; asserting on
    // the icon-only header button specifically would be brittle, so assert the
    // count instead — exactly one, the decision panel's.
    expect(screen.getAllByRole('button', { name: 'Mark complete' })).toHaveLength(1);
  });

  it('routes 重試 through tasks.goal_decide — the same fail-closed path as the channel buttons', async () => {
    const user = userEvent.setup();
    renderAt('task-aaaa1111');
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
        task_id: 'task-aaaa1111',
        action: 'retry',
        note: '',
      }),
    );
    // Never the bare status write (2026-08-14: that route left stale
    // claim/lease behind and leaked the old judge feedback into the next round).
    const updates = mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.update');
    expect(updates).toHaveLength(0);
  });

  it('leaves an ordinary task alone (guard is not a blanket freeze)', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [TASK], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [TASK], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
  });
});
