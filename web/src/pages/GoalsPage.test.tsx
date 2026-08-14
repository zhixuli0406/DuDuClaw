import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { GoalsPage } from './GoalsPage';

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
    expect(screen.getAllByRole('button', { name: /Assign goal/ }).length).toBeGreaterThan(0);
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
