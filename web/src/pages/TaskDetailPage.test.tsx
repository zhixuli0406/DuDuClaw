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

// ── I-1c 想一想 (plan-first): a task parked `needs_human` with `plan_pending`
// set gets a distinct plan card + copy instead of the generic 「卡住原因」
// line, but reuses the exact same three-button decision (no new button kind).

const PLAN_PENDING: TaskInfo = {
  ...TASK,
  status: 'needs_human',
  judge_feedback: '- Search the vendor catalog\n- Draft a comparison table',
  plan_pending: '- Search the vendor catalog\n- Draft a comparison table',
};

describe('TaskDetailPage — 想一想 plan-first (I-1c)', () => {
  beforeEach(() => {
    mockWsClient.call.mockResolvedValue({ tasks: [PLAN_PENDING], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [PLAN_PENDING], comments: {}, activities: [], loading: false });
  });

  it('shows the plan-approval copy and the plan body, not the generic needs-human line', () => {
    renderAt('task-aaaa1111');
    expect(
      screen.getByText('The AI employee drafted an execution plan — it will only start once you approve it.'),
    ).toBeInTheDocument();
    // Testing-library's default whitespace normalizer would collapse the
    // embedded newline, so match the plan body's rendered node directly
    // instead of a string containing "\n".
    expect(
      screen.getByText((_, el) => el?.textContent === PLAN_PENDING.plan_pending),
    ).toBeInTheDocument();
    // The generic "waiting on your decision" prompt is replaced, not stacked.
    expect(
      screen.queryByText('This task is waiting on your decision: try again, call it done, or give up?'),
    ).toBeNull();
  });

  it('still offers the same three decision buttons — no new button kind', () => {
    renderAt('task-aaaa1111');
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Give up' })).toBeInTheDocument();
  });

  it('approving (重試) routes through the SAME tasks.goal_decide retry path', async () => {
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
  });
});

// ── I-3a: 已完成／失敗可續推 — a goal-mode task that already reached a
// terminal state can take a follow-up message and get reopened for another
// round instead of being a dead end (design doc §3.3).

const GOAL_DONE: TaskInfo = { ...TASK, status: 'done', goal_mode: true };
const GOAL_FAILED: TaskInfo = { ...TASK, status: 'failed', goal_mode: true };
const GOAL_CANCELLED: TaskInfo = { ...TASK, status: 'cancelled', goal_mode: true };
const BOARD_DONE: TaskInfo = { ...TASK, status: 'done', goal_mode: false };

describe('TaskDetailPage — 接著做 (I-3a)', () => {
  it('renders the continue panel for a done goal task', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [GOAL_DONE], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [GOAL_DONE], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.getByRole('textbox', { name: 'Continue' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Continue' })).toBeInTheDocument();
  });

  it('renders the continue panel for a failed goal task', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [GOAL_FAILED], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [GOAL_FAILED], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.getByRole('textbox', { name: 'Continue' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Continue' })).toBeInTheDocument();
  });

  it('renders the continue panel for a cancelled goal task', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [GOAL_CANCELLED], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [GOAL_CANCELLED], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.getByRole('textbox', { name: 'Continue' })).toBeInTheDocument();
  });

  it('hides the continue panel for a done task that is not goal-mode (an ordinary board task)', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [BOARD_DONE], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [BOARD_DONE], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.queryByRole('textbox', { name: 'Continue' })).toBeNull();
  });

  it('hides the continue panel for a live (non-terminal) goal task', () => {
    const live: TaskInfo = { ...TASK, status: 'in_progress', goal_mode: true };
    mockWsClient.call.mockResolvedValue({ tasks: [live], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [live], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.queryByRole('textbox', { name: 'Continue' })).toBeNull();
  });

  it('disables submit until a message is typed, then routes through tasks.goal_decide with action: continue', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ tasks: [GOAL_DONE], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [GOAL_DONE], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');

    const submitBtn = screen.getByRole('button', { name: 'Continue' });
    expect(submitBtn).toBeDisabled();

    await user.type(screen.getByRole('textbox', { name: 'Continue' }), 'Please also email the summary to Sam');
    expect(submitBtn).toBeEnabled();

    mockWsClient.call.mockResolvedValueOnce({ ok: true, message: 'queued', task: GOAL_DONE });
    await user.click(submitBtn);
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
        task_id: 'task-aaaa1111',
        action: 'continue',
        message: 'Please also email the summary to Sam',
      }),
    );
  });
});

// ── I-2c: the `/goals?task=` dialog's content merged into this page ────────
// (`GoalLoopPanel` — pause-reason chip, take-over action, contract cards,
// pending kickoff approval, round timeline). Gated on `task.goal_mode` so a
// plain `blocked` board task's decision flow is byte-identical to before.

const NEEDS_HUMAN_GOAL: TaskInfo = {
  ...TASK,
  status: 'needs_human',
  goal_mode: true,
  judge_feedback: 'Could not confirm the refund amount',
  pause_reason: 'no_progress',
};

describe('TaskDetailPage — I-2c goal-loop merge: needs_human take-over', () => {
  it('adds the H11 pause-reason chip and a 4th 「交給我」take-over action for a goal-mode task', () => {
    mockWsClient.call.mockResolvedValue({ tasks: [NEEDS_HUMAN_GOAL], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [NEEDS_HUMAN_GOAL], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    // The chip (H11 classification) — free of the 3 standard buttons still there.
    expect(screen.getByText('Stuck, no progress')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Give up' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: "I'll take over" })).toBeInTheDocument();
  });

  it('leaves the non-goal needs_human flow untouched — no chip, no 4th button', () => {
    // NEEDS_HUMAN (module-level fixture above) has no `goal_mode` set.
    mockWsClient.call.mockResolvedValue({ tasks: [NEEDS_HUMAN], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [NEEDS_HUMAN], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.queryByText('Stuck, no progress')).toBeNull();
    expect(screen.queryByRole('button', { name: "I'll take over" })).toBeNull();
  });

  it('takeover routes through tasks.goal_decide with action: takeover', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ tasks: [NEEDS_HUMAN_GOAL], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [NEEDS_HUMAN_GOAL], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    mockWsClient.call.mockResolvedValueOnce({ ok: true, message: 'taken over', task: NEEDS_HUMAN_GOAL });
    await user.click(screen.getByRole('button', { name: "I'll take over" }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
        task_id: 'task-aaaa1111',
        action: 'takeover',
        note: '',
      }),
    );
  });
});

describe('TaskDetailPage — I-2c goal-loop merge: contract cards + pending kickoff', () => {
  it('renders the acceptance criteria and risk boundary cards for a goal task that carries them', () => {
    const goalWithContract: TaskInfo = {
      ...TASK,
      goal_mode: true,
      acceptance_criteria: 'Ship the Q3 report',
      risk_boundary: 'Never touch production billing',
    };
    mockWsClient.call.mockResolvedValue({ tasks: [goalWithContract], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [goalWithContract], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.getByText('Acceptance criteria')).toBeInTheDocument();
    expect(screen.getByText('Ship the Q3 report')).toBeInTheDocument();
    expect(screen.getByText('Risk boundary for this goal')).toBeInTheDocument();
    expect(screen.getByText('Never touch production billing')).toBeInTheDocument();
  });

  it('renders nothing extra for a goal task with no contract fields set', () => {
    const bareGoal: TaskInfo = { ...TASK, goal_mode: true };
    mockWsClient.call.mockResolvedValue({ tasks: [bareGoal], agents: AGENTS, events: [], comments: [] });
    useTasksStore.setState({ tasks: [bareGoal], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');
    expect(screen.queryByText('Acceptance criteria')).toBeNull();
  });

  it('shows the pending-kickoff approval card and decides via approvals.decide', async () => {
    const user = userEvent.setup();
    const kickoffGoal: TaskInfo = { ...TASK, goal_mode: true, status: 'pending' };
    mockWsClient.call.mockImplementation((method: string) => {
      switch (method) {
        case 'tasks.list':
          return Promise.resolve({ tasks: [kickoffGoal] });
        case 'agents.list':
          return Promise.resolve({ agents: AGENTS });
        case 'tasks.timeline':
          return Promise.resolve({
            task: kickoffGoal,
            iterations: [],
            activity: [],
            runs: [],
            pending_kickoff: {
              id: 'appr-1',
              summary: 'Waiting for you to approve the first dispatch',
              created_at: '2026-07-17T00:00:00Z',
              ttl_seconds: 3600,
            },
          });
        default:
          return Promise.resolve({});
      }
    });
    useTasksStore.setState({ tasks: [kickoffGoal], comments: {}, activities: [], loading: false });
    renderAt('task-aaaa1111');

    expect(await screen.findByText('Kickoff needs your approval')).toBeInTheDocument();
    expect(screen.getByText('Waiting for you to approve the first dispatch')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Approve' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', { id: 'appr-1', approve: true });
    });
  });
});

describe('TaskDetailPage — I-3b pin/archive from the detail kebab', () => {
  it('pins the current task via the kebab, routing through tasks.pin', async () => {
    const user = userEvent.setup();
    renderAt('task-aaaa1111');
    await user.click(screen.getByRole('button', { name: 'More actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Pin' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.pin', { task_id: 'task-aaaa1111' });
    });
  });

  it('archives the current task via the kebab, routing through tasks.archive', async () => {
    const user = userEvent.setup();
    renderAt('task-aaaa1111');
    await user.click(screen.getByRole('button', { name: 'More actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Archive' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.archive', { task_id: 'task-aaaa1111' });
    });
  });
});
