import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Routes, Route } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TaskBoardPage } from './TaskBoardPage';
import { useTasksStore } from '@/stores/tasks-store';
import { useAgentsStore } from '@/stores/agents-store';
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
