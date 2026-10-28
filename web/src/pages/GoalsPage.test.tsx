import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { GoalsPage } from './GoalsPage';
import type { TaskInfo } from '@/lib/api';

// I-2c: `/goals` no longer opens a page-local detail dialog — a task click
// navigates straight to `/tasks/:id`, and `?task=<id>` (the /assign page's landing
// URL after `tasks.goal_create`) is a redirect-only compatibility shim. Both
// need to observe `useNavigate` calls, so the whole module is mocked here
// (same pattern as `AssignPage.test.tsx`) — `useSearchParams` is left as the
// real implementation via `...actual` so `?task=` still comes from the router.
const mockNavigate = vi.fn();
vi.mock('react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-router')>();
  return { ...actual, useNavigate: () => mockNavigate };
});

/** Renders `GoalsPage` at a specific URL (for the `?task=` redirect test) —
 *  `renderWithProviders` doesn't take `initialEntries`. */
function renderGoalsAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <GoalsPage />
      </MemoryRouter>
    </IntlProvider>,
  );
}

const goalTask = {
  id: 'gt-1',
  title: '整理客戶月報',
  description: '把客戶資料整理成月報',
  status: 'needs_human',
  priority: 'medium',
  assigned_to: 'agnes',
  created_by: 'goal:dashboard',
  created_at: '2026-08-14T00:00:00Z',
  updated_at: '2026-08-14T01:00:00Z',
  tags: [],
  goal_mode: true,
  revision_round: 2,
  judge_feedback: '缺少營收圖表',
  pause_reason: 'no_progress',
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockImplementation((method: string) => {
    switch (method) {
      case 'tasks.list':
        return Promise.resolve({ tasks: [goalTask, { ...goalTask, id: 'nt-1', goal_mode: false }] });
      case 'agents.list':
        return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
      case 'tasks.goal_decide':
        return Promise.resolve({ ok: true, message: '重試此目標任務。', task: goalTask });
      default:
        return Promise.resolve({});
    }
  });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('GoalsPage', () => {
  it('renders the header and an assign action', async () => {
    renderWithProviders(<GoalsPage />);
    expect(screen.getByRole('heading', { name: 'Goals' })).toBeInTheDocument();
    // One verb across the app (UX plan I-1a): this button used to say
    // "Assign goal" and open a page-local seven-field dialog; it now opens the
    // shared /assign page.
    expect(screen.getAllByRole('button', { name: /Assign task/ }).length).toBeGreaterThan(0);
  });

  it('lists only goal_mode tasks, grouped into the needs-you section', async () => {
    renderWithProviders(<GoalsPage />);
    expect(await screen.findByText('整理客戶月報')).toBeInTheDocument();
    // The non-goal board task is filtered out entirely.
    expect(screen.getAllByText('整理客戶月報')).toHaveLength(1);
    expect(screen.getByText('Needs your decision (1)')).toBeInTheDocument();
    // Judge feedback preview + inline intervention buttons on the card.
    expect(screen.getByText('缺少營收圖表')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Retry/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Mark done/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /take over/i })).toBeInTheDocument();
  });

  // H11: the free-text feedback says WHAT the judge complained about; the chip
  // says what KIND of stop this was, which is what a person triages on.
  it('shows the pause-reason chip on a needs_human card', async () => {
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    expect(screen.getByText('Stuck, no progress')).toBeInTheDocument();
  });

  it('falls back to the unknown chip for a legacy or unrecognised pause reason', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      switch (method) {
        case 'tasks.list':
          // No `pause_reason` at all — a row written before H11 existed.
          return Promise.resolve({ tasks: [{ ...goalTask, pause_reason: undefined }] });
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
        default:
          return Promise.resolve({});
      }
    });
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    expect(screen.getByText('Needs a human to check')).toBeInTheDocument();
  });

  it('routes a needs_human decision through tasks.goal_decide, never tasks.update', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByRole('button', { name: /Mark done/ }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', {
        task_id: 'gt-1',
        action: 'done',
        note: '',
      });
    });
    const updateCalls = mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.update');
    expect(updateCalls).toHaveLength(0);
  });
});

// ── I-2c: `/goals` opens the SAME detail surface as everywhere else ────────

describe('GoalsPage — I-2c detail navigation (no local dialog)', () => {
  it('opens a task by navigating to /tasks/:id instead of a page-local dialog', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByText('整理客戶月報'));
    expect(mockNavigate).toHaveBeenCalledWith('/tasks/gt-1');
    // The old `GoalDetailDialog` never mounts — opening a task is pure
    // navigation now, not a page-local dialog.
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('redirects a legacy /goals?task= entry point to /tasks/:id (/assign landing compat)', async () => {
    renderGoalsAt('/goals?task=gt-9');
    await waitFor(() => {
      expect(mockNavigate).toHaveBeenCalledWith('/tasks/gt-9', { replace: true });
    });
  });
});

// ── I-3b: task list operations (pin / rename / archive) ────────────────────

describe('GoalsPage — I-3b list actions', () => {
  it('pins a task via the row kebab, routing through tasks.pin', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByRole('button', { name: 'Task actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Pin' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.pin', { task_id: 'gt-1' });
    });
  });

  it('unpins an already-pinned task, routing through tasks.unpin', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      switch (method) {
        case 'tasks.list':
          return Promise.resolve({ tasks: [{ ...goalTask, pinned: true }] });
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
        default:
          return Promise.resolve({});
      }
    });
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByRole('button', { name: 'Task actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Unpin' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.unpin', { task_id: 'gt-1' });
    });
  });

  it('archives a task via the row kebab, routing through tasks.archive', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByRole('button', { name: 'Task actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Archive' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.archive', { task_id: 'gt-1' });
    });
  });

  it('renames a task via the kebab dialog, routing through tasks.rename', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.click(screen.getByRole('button', { name: 'Task actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Rename' }));

    const input = await screen.findByRole('textbox', { name: 'Rename task' });
    await user.clear(input);
    await user.type(input, '新的標題');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.rename', {
        task_id: 'gt-1',
        title: '新的標題',
      });
    });
  });

  it('shows the 顯示已封存 toggle and the search box in the filter bar', async () => {
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    expect(screen.getByRole('switch', { name: 'Show archived' })).toBeInTheDocument();
    expect(screen.getByPlaceholderText('Search task titles…')).toBeInTheDocument();
  });

  it('filters every section by the search box (front-end title match)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await screen.findByText('整理客戶月報');
    await user.type(screen.getByPlaceholderText('Search task titles…'), 'nonexistent-xyz');
    expect(await screen.findByText('No tasks match your search.')).toBeInTheDocument();
    expect(screen.queryByText('整理客戶月報')).not.toBeInTheDocument();
  });
});

// ── I-3b: 已結束 pagination replaces the old `.slice(0, 20)` hard cutoff ────

function finishedTask(i: number): TaskInfo {
  return {
    id: `done-${i}`,
    title: `Finished goal ${i}`,
    description: '',
    status: 'done',
    priority: 'medium',
    assigned_to: 'agnes',
    created_by: 'goal:dashboard',
    created_at: '2026-08-01T00:00:00Z',
    updated_at: `2026-08-01T00:${String(i).padStart(2, '0')}:00Z`,
    tags: [],
    goal_mode: true,
  };
}

describe('GoalsPage — I-3b 已結束 pagination (no more hard 20-item cutoff)', () => {
  const allDone = Array.from({ length: 25 }, (_, i) => finishedTask(i));

  beforeEach(() => {
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      switch (method) {
        case 'tasks.list':
          // No active/waiting goal tasks — isolates this test to the
          // finished/archived pagination path.
          return Promise.resolve({ tasks: [] });
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
        case 'tasks.list_page': {
          if (params?.status !== 'done') return Promise.resolve({ tasks: [], total: 0 });
          const offset = Number(params.offset ?? 0);
          const limit = Number(params.limit ?? 20);
          return Promise.resolve({ tasks: allDone.slice(offset, offset + limit), total: allDone.length });
        }
        default:
          return Promise.resolve({});
      }
    });
  });

  it('loads the first page (20) and reveals more via "Load more" instead of a hard cutoff', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);

    await screen.findByText('Finished goal 0');
    expect(screen.getByText('Finished goal 19')).toBeInTheDocument();
    // The old `.slice(0, 20)` hard cutoff made the 21st item unreachable —
    // it now just waits behind "Load more".
    expect(screen.queryByText('Finished goal 20')).not.toBeInTheDocument();

    const loadMore = screen.getByRole('button', { name: 'Load more' });
    await user.click(loadMore);

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.list_page',
        expect.objectContaining({ status: 'done', offset: 20 }),
      );
    });
    expect(await screen.findByText('Finished goal 20')).toBeInTheDocument();
    expect(screen.getByText('Finished goal 24')).toBeInTheDocument();
    // All 25 loaded now — "Load more" retires.
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
  });
});

it('keeps discovery tasks out of ordinary goal cards even when a legacy row has goal_mode set', async () => {
  const original=mockWsClient.call.getMockImplementation();
  mockWsClient.call.mockImplementation((method:string,...params:unknown[])=>method==='tasks.list'
    ? Promise.resolve({tasks:[goalTask,{...goalTask,id:'discovery-legacy',title:'Must not use goal intervention',kind:'discovery',goal_mode:true}]})
    : original?.(method,...params));
  renderWithProviders(<GoalsPage />);
  expect(await screen.findByText('整理客戶月報')).toBeInTheDocument();
  expect(screen.queryByText('Must not use goal intervention')).not.toBeInTheDocument();
});

describe('GoalsPage — assigned by me', () => {
  it('opens the "assigned by me" view from ?assigned=me and lists through on_behalf_of', async () => {
    const mineTask = { ...goalTask, id: 'mine-1', title: '整理報價單', status: 'in_progress', on_behalf_of: 'user:u1', created_via: 'line' };
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'tasks.list_page') {
        const isMine = params?.on_behalf_of === 'me';
        const unfinished = !params?.status;
        return Promise.resolve({ tasks: isMine && unfinished ? [mineTask] : [], total: isMine && unfinished ? 1 : 0, limit: 50, offset: 0 });
      }
      if (method === 'agents.list') return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
      return Promise.resolve({});
    });
    renderGoalsAt('/goals?assigned=me');
    expect(await screen.findByText('整理報價單')).toBeInTheDocument();
    const calls = mockWsClient.call.mock.calls.filter(([m]) => m === 'tasks.list_page');
    expect(calls.length).toBeGreaterThan(0);
    for (const [, p] of calls) {
      expect((p as Record<string, unknown>).on_behalf_of).toBe('me');
      expect((p as Record<string, unknown>).agent_id).toBeUndefined();
    }
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'tasks.list')).toBe(false);
    expect(screen.getAllByText('Assigned by me').length).toBeGreaterThan(0);
  });
});


describe('GoalsPage — goal cost cap', () => {
  it('shows a deferred configuration notice even on an uncapped goal awaiting dispatch', async () => {
    const notice = '目標花費上限設定無法讀取或格式錯誤，已暫停派工。請檢查 [goal_loop] default_cost_cap_cents（須為非負整數，0 代表不限額）。';
    const original = mockWsClient.call.getMockImplementation();
    mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) => method === 'tasks.list'
      ? Promise.resolve({ tasks: [{ ...goalTask, status: 'todo', cost_cap_cents: null, effective_cost_cap_cents: null, blocked_reason: notice }] })
      : original?.(method, ...args));
    renderWithProviders(<GoalsPage />);
    expect(await screen.findByText(notice)).toBeInTheDocument();
    expect(screen.queryByText(/Spent \$0.00/)).not.toBeInTheDocument();
  });

  function costGoal(spent: number | null = 250) {
    const original = mockWsClient.call.getMockImplementation();
    mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) =>
      method === 'tasks.list'
        ? Promise.resolve({ tasks: [{ ...goalTask, cost_cap_cents: null, effective_cost_cap_cents: 200, measured_spent_cents: spent, pause_reason: 'budget_exhausted', judge_feedback: 'Spent 250 cents; cap 200 cents.' }] })
        : original?.(method, ...args));
  }

  it('shows measured spend against the effective cap and the complete pause detail', async () => {
    costGoal();
    renderWithProviders(<GoalsPage />);
    expect(await screen.findByText('Spent $2.50 / $2.00 cap')).toBeInTheDocument();
    expect(screen.getByText('Spent 250 cents; cap 200 cents.')).toBeInTheDocument();
    expect(screen.getByText('Rounds, time or cost limit reached')).toBeInTheDocument();
  });

  it('shows unavailable spend explicitly instead of displaying zero', async () => {
    costGoal(null);
    renderWithProviders(<GoalsPage />);
    expect(await screen.findByText('Spend unavailable / $2.00 cap')).toBeInTheDocument();
    expect(screen.queryByText('Spent $0.00 / $2.00 cap')).not.toBeInTheDocument();
  });

  it('raises a paused cap before retrying through the existing decision gate', async () => {
    costGoal();
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    const input = await screen.findByRole('spinbutton', { name: 'New cost cap (US cents)' });
    await user.type(input, '400');
    await user.click(screen.getByRole('button', { name: 'Raise cap and retry' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('tasks.update', { task_id: 'gt-1', cost_cap_cents: 400 }));
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_decide', { task_id: 'gt-1', action: 'retry', note: '' });
    const methods = mockWsClient.call.mock.calls.map(([method]) => method);
    expect(methods.indexOf('tasks.update')).toBeLessThan(methods.indexOf('tasks.goal_decide'));
  });

  it('shows spend and cap for finished goals too', async () => {
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'tasks.list') return Promise.resolve({ tasks: [] });
      if (method === 'tasks.list_page') return Promise.resolve({ tasks: params?.status === 'done'
        ? [{ ...goalTask, status: 'done', cost_cap_cents: 500, measured_spent_cents: 100 }] : [], total: params?.status === 'done' ? 1 : 0 });
      if (method === 'agents.list') return Promise.resolve({ agents: [{ name: 'agnes' }] });
      return Promise.resolve({});
    });
    renderWithProviders(<GoalsPage />);
    expect(await screen.findByText('Spent $1.00 / $5.00 cap')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Raise cap and retry' })).not.toBeInTheDocument();
  });

  it('does not retry when saving the raised cap fails', async () => {
    costGoal();
    const original = mockWsClient.call.getMockImplementation();
    mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) => method === 'tasks.update'
      ? Promise.reject(new Error('Not authorized')) : original?.(method, ...args));
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    await user.type(await screen.findByRole('spinbutton', { name: 'New cost cap (US cents)' }), '400');
    await user.click(screen.getByRole('button', { name: 'Raise cap and retry' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('tasks.update', expect.anything()));
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'tasks.goal_decide')).toBe(false);
  });

  it('rejects a cap that is not an integer above both current spend and cap', async () => {
    costGoal();
    const user = userEvent.setup();
    renderWithProviders(<GoalsPage />);
    const input = await screen.findByRole('spinbutton', { name: 'New cost cap (US cents)' });
    const save = screen.getByRole('button', { name: 'Raise cap and retry' });
    for (const value of ['200', '250', '250.5', '9007199254740992']) {
      await user.clear(input);
      await user.type(input, value);
      expect(save).toBeDisabled();
    }
  });
});
