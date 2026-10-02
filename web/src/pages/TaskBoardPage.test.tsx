import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TaskBoardPage } from './TaskBoardPage';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
import { toastBus, type ToastInput } from '@/lib/toast';
import type { TaskInfo } from '@/lib/api';

const AGENTS = [{ name: 'nova', display_name: 'Nova', status: 'active', role: 'main', sandboxed: false }];

function task(over: Partial<TaskInfo>): TaskInfo {
  return {
    id: 'task-0001',
    title: 'Untitled',
    description: '',
    status: 'todo',
    priority: 'medium',
    assigned_to: 'nova',
    created_by: 'user',
    created_at: '2026-07-17T00:00:00Z',
    updated_at: '2026-07-17T00:00:00Z',
    tags: [],
    ...over,
  };
}

const SEED: TaskInfo[] = [
  task({ id: 'task-aaaa1111', title: 'Draft the launch plan', status: 'todo', priority: 'high' }),
  task({ id: 'task-bbbb2222', title: 'Ship the release', status: 'in_progress', priority: 'urgent' }),
];

beforeEach(() => {
  vi.clearAllMocks();
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
  // Combined payload satisfies tasks.list / agents.list / activity.list, whose
  // reducers each read only their own key.
  mockWsClient.call.mockResolvedValue({ tasks: SEED, agents: AGENTS, events: [] });
  useTasksStore.setState({ tasks: SEED, loading: false, filterAgent: null, filterPriority: null });
  useAgentsStore.setState({ agents: AGENTS as never[], loading: false });
});

describe('TaskBoardPage', () => {
  it('renders the two-layer header with title and create action', () => {
    renderWithProviders(<TaskBoardPage />);
    expect(screen.getByRole('heading', { name: 'Task Board' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'New Task' })).toBeInTheDocument();
  });

  it('shows the empty state when there are no tasks', async () => {
    useTasksStore.setState({ tasks: [], loading: false });
    mockWsClient.call.mockResolvedValue({ tasks: [], agents: AGENTS, events: [] });
    renderWithProviders(<TaskBoardPage />);
    await waitFor(() => {
      expect(
        screen.getByText('No tasks yet. Create your first task to start managing work!'),
      ).toBeInTheDocument();
    });
  });

  it('renders tasks on the board (default kanban view)', () => {
    renderWithProviders(<TaskBoardPage />);
    expect(screen.getByText('Draft the launch plan')).toBeInTheDocument();
    expect(screen.getByText('Ship the release')).toBeInTheDocument();
    // Board columns are labelled by status.
    expect(screen.getByRole('heading', { name: 'To Do' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'In Progress' })).toBeInTheDocument();
  });

  it('switches to list view and persists the preference', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TaskBoardPage />);

    await user.click(screen.getByRole('radio', { name: 'List' }));

    // The list-view title renders as a clickable button.
    expect(await screen.findByRole('button', { name: 'Draft the launch plan' })).toBeInTheDocument();
    expect(localStorage.getItem('duduclaw:tasks:view')).toBe('list');
  });

  it('navigates to the task detail when a list row title is clicked', async () => {
    const user = userEvent.setup();
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(
      <Routes>
        <Route path="/" element={<TaskBoardPage />} />
        <Route path="/tasks/:id" element={<div>detail-probe</div>} />
      </Routes>,
    );

    await user.click(await screen.findByRole('button', { name: 'Ship the release' }));
    await waitFor(() => {
      expect(screen.getByText('detail-probe')).toBeInTheDocument();
    });
  });
});

// ── W3-3: state-as-URL (Stripe pattern B4) — the agent/priority filters are
// bookmarkable/shareable, matching the existing `?new=1` / `?assignee=` deep
// links this page already supports.

/** Shows the current query string, for asserting an outbound URL mirror. */
function SearchParamsProbe() {
  const [params] = useSearchParams();
  return <div data-testid="search-probe">{params.toString()}</div>;
}

/** `renderWithProviders`'s router has no way to seed a starting location/query
 *  string — same limitation InboxPage's own deep-link tests work around. */
function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route
            path="/tasks"
            element={
              <>
                <TaskBoardPage />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('TaskBoardPage — filters as URL (W3-3)', () => {
  it('seeds the agent/priority filters from ?agent=/?priority= on load', async () => {
    renderAt('/tasks?agent=nova&priority=high');
    await waitFor(() => {
      expect(useTasksStore.getState().filterAgent).toBe('nova');
      expect(useTasksStore.getState().filterPriority).toBe('high');
    });
  });

  it('ignores an unrecognized ?priority= value rather than coercing it', async () => {
    renderAt('/tasks?priority=not-a-real-priority');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Task Board' })).toBeInTheDocument();
    });
    expect(useTasksStore.getState().filterPriority).toBeNull();
  });

  it('mirrors an in-app filter change back into the URL', async () => {
    const user = userEvent.setup();
    renderAt('/tasks');

    await user.click(screen.getByRole('button', { name: /Filter/ }));
    await user.click(await screen.findByRole('menuitem', { name: /Nova/ }));

    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('agent=nova');
    });
  });

  it('does not carry a bookmarked filter forward once cleared (no stale param)', async () => {
    const user = userEvent.setup();
    renderAt('/tasks?agent=nova');
    await waitFor(() => expect(useTasksStore.getState().filterAgent).toBe('nova'));

    await user.click(screen.getByRole('button', { name: /Filter/ }));
    await user.click(await screen.findByRole('menuitem', { name: 'Clear filters' }));

    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).not.toHaveTextContent('agent=nova');
    });
  });
});

// ── WP-A (§2-6): a task parked on a human decision ──────────────────────────
// The board used to show it as a read-only badge in a read-only column, so the
// only thing you could do with it there was drag it somewhere else — which the
// board happily did, skipping the decision entirely. Now the card carries the
// decision, and every other writer path refuses the task.

const NEEDS_HUMAN: TaskInfo = task({
  id: 'task-cccc3333',
  title: 'Waiting on you',
  status: 'needs_human',
  judge_feedback: 'Could not confirm the refund amount',
});

/** Capture toasts — `renderWithProviders` mounts no ToastProvider. */
function captureToasts() {
  const seen: ToastInput[] = [];
  const off = toastBus.subscribe((t) => seen.push(t));
  return { seen, off };
}

describe('TaskBoardPage — 等你決定 is decided, never moved (WP-A §2-6)', () => {
  beforeEach(() => {
    useTasksStore.setState({ tasks: [...SEED, NEEDS_HUMAN], loading: false });
    mockWsClient.call.mockResolvedValue({ tasks: [...SEED, NEEDS_HUMAN], agents: AGENTS, events: [] });
  });

  it('puts the same three choices on the card as the inbox offers', () => {
    renderWithProviders(<TaskBoardPage />);
    expect(screen.getByText('Waiting on you')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Mark complete' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Give up' })).toBeInTheDocument();
    // The escalation reason stays visible next to the decision.
    expect(screen.getByText(/Could not confirm the refund amount/)).toBeInTheDocument();
  });

  it('refuses a drop into a writable column and points at the inbox', async () => {
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    const { seen, off } = captureToasts();
    renderWithProviders(<TaskBoardPage />);

    // The droppable body is the sibling of each column's header row.
    const doneColumn = screen.getByRole('heading', { name: 'Done' }).closest('div')!
      .nextElementSibling as HTMLElement;
    fireEvent.drop(doneColumn, {
      dataTransfer: { getData: () => NEEDS_HUMAN.id },
    });

    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0].message).toBe('This task is waiting on your decision — handle it in the inbox');
    expect(seen[0].action?.label).toBe('Go to inbox');
    expect(moveTask).not.toHaveBeenCalled();
    off();
  });

  it('still moves an ordinary card, so the guard is not a blanket freeze', async () => {
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    renderWithProviders(<TaskBoardPage />);

    const doneColumn = screen.getByRole('heading', { name: 'Done' }).closest('div')!
      .nextElementSibling as HTMLElement;
    fireEvent.drop(doneColumn, { dataTransfer: { getData: () => 'task-aaaa1111' } });

    await waitFor(() => expect(moveTask).toHaveBeenCalledWith('task-aaaa1111', 'done'));
  });

  it('refuses the list view’s inline status picker too (same guard, other path)', async () => {
    const user = userEvent.setup();
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    const { seen, off } = captureToasts();
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(<TaskBoardPage />);

    await user.click(await screen.findByRole('button', { name: 'Needs your decision' }));
    await user.click(await screen.findByRole('menuitemradio', { name: 'Done' }));

    await waitFor(() => expect(seen).toHaveLength(1));
    expect(moveTask).not.toHaveBeenCalled();
    off();
  });
});

describe('TaskBoardPage — the batch action cannot route around the guard', () => {
  beforeEach(() => {
    useTasksStore.setState({ tasks: [SEED[0], NEEDS_HUMAN], loading: false });
    mockWsClient.call.mockResolvedValue({ tasks: [SEED[0], NEEDS_HUMAN], agents: AGENTS, events: [] });
  });

  it('completes the ordinary selection and refuses only the 等你決定 row', async () => {
    const user = userEvent.setup();
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    const { seen, off } = captureToasts();
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(<TaskBoardPage />);

    for (const cb of await screen.findAllByRole('checkbox', { name: 'Select task' })) {
      await user.click(cb);
    }
    await user.click(await screen.findByRole('button', { name: /Mark done/i }));

    await waitFor(() => expect(moveTask).toHaveBeenCalledWith('task-aaaa1111', 'done'));
    expect(moveTask).not.toHaveBeenCalledWith(NEEDS_HUMAN.id, 'done');
    expect(seen.some((t) => t.message.includes('waiting on your decision'))).toBe(true);
    off();
  });
});

describe('TaskBoardPage — a failed delete is reported, not mimed as success', () => {
  beforeEach(() => {
    useTasksStore.setState({ tasks: [...SEED], loading: false, error: null });
    mockWsClient.call.mockResolvedValue({ tasks: [...SEED], agents: AGENTS, events: [] });
  });

  /** Open the single-task delete confirmation from the first card. */
  async function openDeleteDialog(user: ReturnType<typeof userEvent.setup>) {
    renderWithProviders(<TaskBoardPage />);
    const trash = (await screen.findAllByRole('button', { name: 'Delete Task' }))[0];
    await user.click(trash);
    return await screen.findByRole('dialog');
  }

  // Nothing in this file read the store's `error`, so a rejected delete closed
  // the confirmation dialog and left the user believing the task was gone
  // (P05 Blocker, phase-4 audit).
  it('keeps the confirmation open and says so when the delete fails', async () => {
    const user = userEvent.setup();
    const { seen, off } = captureToasts();
    const removeTask = vi.fn().mockImplementation(async () => {
      useTasksStore.setState({ error: new Error('Failed to fetch') });
    });
    useTasksStore.setState({ removeTask } as never);

    const dialog = await openDeleteDialog(user);
    await user.click(within(dialog).getByRole('button', { name: 'Delete Task' }));

    await waitFor(() => expect(removeTask).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0].variant).toBe('error');
    // The dialog stays up: the task is still there and the user must know.
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    off();
  });

  it('closes the confirmation when the delete lands', async () => {
    const user = userEvent.setup();
    const removeTask = vi.fn().mockResolvedValue(undefined);
    useTasksStore.setState({ removeTask, error: null } as never);

    const dialog = await openDeleteDialog(user);
    await user.click(within(dialog).getByRole('button', { name: 'Delete Task' }));

    await waitFor(() => expect(removeTask).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });
});

// ── L2 round 3: non-admin fan-out — WIP gauge + partial-load notice ──
import { useAuthStore } from '@/stores/auth-store';

const PARTIAL_NOTICE = "Some AI employees' data couldn't be loaded this time — showing what did load.";

function signInManager(agents: string[]) {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u@x', display_name: 'U', role: 'manager', status: 'active' },
    bindings: agents.map((agent_name) => ({ user_id: 'u1', agent_name, access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' })),
    isAuthenticated: true,
  });
}

describe('TaskBoardPage for a non-admin (L2)', () => {
  beforeEach(() => {
    useTasksStore.setState({ tasks: [], loading: false, error: null, tasksPartialError: null });
  });
  afterEach(() => {
    useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  });

  function gateway(failAgent?: string) {
    return (method: string, params?: Record<string, unknown>) => {
      const agent = params?.agent_id as string | undefined;
      if (method === 'tasks.list' || method === 'tasks.flow_metrics') {
        if (!agent) return Promise.reject(new Error('agent_id parameter is required'));
        if (agent === failAgent) return Promise.reject(new Error('backend hiccup'));
      }
      if (method === 'tasks.list') {
        return Promise.resolve({ tasks: [task({ id: `task-${agent}`, title: `Work of ${agent}`, assigned_to: agent })] });
      }
      if (method === 'tasks.flow_metrics') {
        return Promise.resolve({ agents: [], review_queue_depth: 0, review_wip_limit: 5, accepts_last_7d: 0, avg_daily_accepts_7d: 0 });
      }
      return Promise.resolve({ agents: AGENTS, events: [] });
    };
  }

  it('a manager now sees the review-column WIP gauge', async () => {
    signInManager(['nova']);
    mockWsClient.call.mockImplementation(gateway());
    renderWithProviders(<TaskBoardPage />);
    expect(await screen.findByTitle('Review queue: 0 of 5 (WIP limit)')).toBeInTheDocument();
  });

  it('partial result: renders the tasks that loaded plus one status notice', async () => {
    signInManager(['nova', 'zed']);
    mockWsClient.call.mockImplementation(gateway('zed'));
    renderWithProviders(<TaskBoardPage />);
    expect(await screen.findByText('Work of nova')).toBeInTheDocument();
    const notice = await screen.findByText(PARTIAL_NOTICE);
    expect(notice.closest('[role="status"]')).not.toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('clean result: no notice (and a clean load clears an earlier one)', async () => {
    useTasksStore.setState({ tasksPartialError: new Error('earlier') });
    signInManager(['nova', 'zed']);
    mockWsClient.call.mockImplementation(gateway());
    renderWithProviders(<TaskBoardPage />);
    expect(await screen.findByText('Work of zed')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText(PARTIAL_NOTICE)).toBeNull());
  });
});

describe('TaskBoardPage with discovery statuses (L2 round 4)', () => {
  it('puts pending_approval / queued tasks in the To Do column (no crash, no vanish)', async () => {
    const rows = [
      task({ id: 'task-disc0001', title: 'Pending discovery', status: 'pending_approval' as never }),
      task({ id: 'task-disc0002', title: 'Queued discovery', status: 'queued' as never }),
      task({ id: 'task-disc0003', title: 'Mystery state', status: 'from_the_future' as never }),
    ];
    mockWsClient.call.mockResolvedValue({ tasks: rows, agents: AGENTS, events: [] });
    useTasksStore.setState({ tasks: rows, loading: false });
    renderWithProviders(<TaskBoardPage />);
    const heading = await screen.findByRole('heading', { name: 'To Do' });
    const column = heading.closest('.w-70') as HTMLElement;
    expect(within(column).getByText('Pending discovery')).toBeInTheDocument();
    expect(within(column).getByText('Queued discovery')).toBeInTheDocument();
    expect(within(column).getByText('Mystery state')).toBeInTheDocument();
  });
});

describe('TaskBoardPage — discovery tasks are read-only (L2 round 5)', () => {
  const DISC = task({ id: 'task-disc0009', title: 'Exploration run', status: 'queued' as never, kind: 'discovery' });
  const PLAIN = task({ id: 'task-plain009', title: 'Plain work', status: 'todo' });

  beforeEach(() => {
    mockWsClient.call.mockResolvedValue({ tasks: [DISC, PLAIN], agents: AGENTS, events: [] });
    useTasksStore.setState({ tasks: [DISC, PLAIN], loading: false });
  });

  it('kanban: a discovery card is not draggable and says where it is managed; a plain card still is', async () => {
    renderWithProviders(<TaskBoardPage />);
    const disc = (await screen.findByText('Exploration run')).closest('[draggable]') as HTMLElement;
    expect(disc).toHaveAttribute('draggable', 'false');
    expect(disc.getAttribute('title')).toMatch(/managed in the exploration section of the Goals page/);
    const plain = screen.getByText('Plain work').closest('[draggable]') as HTMLElement;
    expect(plain).toHaveAttribute('draggable', 'true');
  });

  it('list: a discovery row has no status picker; a plain row keeps it', async () => {
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(<TaskBoardPage />);
    const discRow = (await screen.findByText('Exploration run')).closest('li') as HTMLElement;
    expect(within(discRow).queryByRole('button', { name: 'To do' })).toBeNull();
    const plainRow = screen.getByText('Plain work').closest('li') as HTMLElement;
    expect(within(plainRow).getByRole('button', { name: 'To do' })).toBeInTheDocument();
  });
});

describe('TaskBoardPage — discovery tasks cannot be deleted; batches skip them (L2 round 6)', () => {
  const D1 = task({ id: 'task-disc0101', title: 'Exploration A', status: 'queued' as never, kind: 'discovery' });
  const D2 = task({ id: 'task-disc0102', title: 'Exploration B', status: 'pending_approval' as never, kind: 'discovery' });
  const P1 = task({ id: 'task-plain101', title: 'Plain one', status: 'todo' });
  const SKIPPED = '2 code explorations were skipped — they are managed in the exploration section of the Goals page.';

  beforeEach(() => {
    mockWsClient.call.mockResolvedValue({ tasks: [D1, D2, P1], agents: AGENTS, events: [] });
    useTasksStore.setState({ tasks: [D1, D2, P1], loading: false, error: null });
  });

  async function selectAllInList() {
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(<TaskBoardPage />);
    const user = userEvent.setup();
    for (const title of ['Exploration A', 'Exploration B', 'Plain one']) {
      const row = (await screen.findByText(title)).closest('li') as HTMLElement;
      await user.click(within(row).getByRole('checkbox'));
    }
    return user;
  }

  it('kanban: a discovery card has no delete button; a plain card keeps it', async () => {
    renderWithProviders(<TaskBoardPage />);
    const disc = (await screen.findByText('Exploration A')).closest('[draggable]') as HTMLElement;
    expect(within(disc).queryByRole('button', { name: 'Delete Task' })).toBeNull();
    const plain = screen.getByText('Plain one').closest('[draggable]') as HTMLElement;
    expect(within(plain).getByRole('button', { name: 'Delete Task' })).toBeInTheDocument();
  });

  it('bulk delete skips discovery tasks, deletes the rest, and says so in ONE toast', async () => {
    const removeTask = vi.fn().mockResolvedValue(undefined);
    useTasksStore.setState({ removeTask } as never);
    const { seen, off } = captureToasts();
    const user = await selectAllInList();
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(removeTask).toHaveBeenCalledTimes(1));
    expect(removeTask).toHaveBeenCalledWith('task-plain101');
    const skipped = seen.filter((t) => t.message === SKIPPED);
    expect(skipped).toHaveLength(1);
    off();
  });

  it('bulk mark-done skips discovery tasks with ONE toast and still completes the rest', async () => {
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    const { seen, off } = captureToasts();
    const user = await selectAllInList();
    await user.click(screen.getByRole('button', { name: 'Mark done' }));
    await waitFor(() => expect(moveTask).toHaveBeenCalledWith('task-plain101', 'done'));
    expect(moveTask).toHaveBeenCalledTimes(1);
    expect(seen.filter((t) => t.message === SKIPPED)).toHaveLength(1);
    expect(seen.filter((t) => /exploration/i.test(t.message))).toHaveLength(1);
    off();
  });
});

describe('TaskBoardPage — batch polish (L2 round 7)', () => {
  const D1 = task({ id: 'task-disc0201', title: 'Exploration X', status: 'queued' as never, kind: 'discovery' });
  const D2 = task({ id: 'task-disc0202', title: 'Exploration Y', status: 'queued' as never, kind: 'discovery' });
  const P1 = task({ id: 'task-plain201', title: 'Plain X', status: 'todo' });
  const SKIPPED_2 = '2 code explorations were skipped — they are managed in the exploration section of the Goals page.';
  const SKIPPED_1 = '1 code exploration was skipped — it is managed in the exploration section of the Goals page.';

  async function selectInList(rows: TaskInfo[]) {
    mockWsClient.call.mockResolvedValue({ tasks: rows, agents: AGENTS, events: [] });
    useTasksStore.setState({ tasks: rows, loading: false, error: null });
    localStorage.setItem('duduclaw:tasks:view', 'list');
    renderWithProviders(<TaskBoardPage />);
    const user = userEvent.setup();
    for (const r of rows) {
      const row = (await screen.findByText(r.title)).closest('li') as HTMLElement;
      await user.click(within(row).getByRole('checkbox'));
    }
    return user;
  }

  it('bulk delete with ONLY discovery selected: no confirmation dialog, one toast, selection cleared', async () => {
    const removeTask = vi.fn();
    useTasksStore.setState({ removeTask } as never);
    const { seen, off } = captureToasts();
    const user = await selectInList([D1, D2]);
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(seen.filter((t) => t.message === SKIPPED_2)).toHaveLength(1);
    expect(removeTask).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: 'Mark done' })).toBeNull(); // toolbar gone = selection cleared
    off();
  });

  it('bulk mark-done with ONLY discovery selected: one toast, nothing written', async () => {
    const moveTask = vi.fn();
    useTasksStore.setState({ moveTask } as never);
    const { seen, off } = captureToasts();
    const user = await selectInList([D1, D2]);
    await user.click(screen.getByRole('button', { name: 'Mark done' }));
    expect(moveTask).not.toHaveBeenCalled();
    expect(seen.filter((t) => t.message === SKIPPED_2)).toHaveLength(1);
    off();
  });

  it('a retry after a partial delete failure does not repeat the skipped toast', async () => {
    const removeTask = vi.fn(async () => {
      useTasksStore.setState({ error: new Error('boom') });
    });
    useTasksStore.setState({ removeTask } as never);
    const { seen, off } = captureToasts();
    const user = await selectInList([D1, P1]);
    await user.click(screen.getByRole('button', { name: 'Delete' }));
    let dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(removeTask).toHaveBeenCalledTimes(1));
    dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(removeTask).toHaveBeenCalledTimes(2));
    expect(seen.filter((t) => t.message === SKIPPED_1)).toHaveLength(1);
    off();
  });
});
