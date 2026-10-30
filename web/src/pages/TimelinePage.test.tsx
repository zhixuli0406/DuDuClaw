import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { AgentDetail, TimelineRow } from '@/lib/api';
import { TimelinePage } from './TimelinePage';

const from = '2026-10-30T00:00:00Z';
const to = '2026-10-31T00:00:00Z';
function task(id: string, agent: string, status: string): TimelineRow {
  return { agent_id: agent, kind: 'task', label: `Task ${id}`, started_at: from, ended_at: status === 'done' ? to : null, status, ref_id: id };
}
function respond(rows: TimelineRow[]) {
  mockWsClient.call.mockImplementation(async (method: string) => method === 'timeline.list'
    ? { rows, from, to, cap: 2000, truncated: false } : {});
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
  useAuthStore.setState({ user: { id: 'owner', display_name: 'Owner', email: '', role: 'admin', status: 'active' } });
  useAgentsStore.setState({ agents: ['active', 'waiting', 'stuck', 'idle'].map((name) => ({ name, display_name: name })) as AgentDetail[] });
  respond([]);
});

describe('TimelinePage', () => {
  it('renders the page purpose and range control', async () => {
    renderWithProviders(<TimelinePage />);
    expect(screen.getByRole('heading', { name: 'Work timeline' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: '24 hours' })).toBeInTheDocument();
    expect(screen.getByText(/See who finished what/)).toBeInTheDocument();
    await screen.findByText('No work recorded in this window');
  });

  it('summarizes tasks once and filters the employee list by clicking a count', async () => {
    respond([task('a', 'active', 'in_progress'), task('b', 'waiting', 'needs_human'), task('c', 'active', 'done'), task('d', 'stuck', 'failed'), task('c', 'active', 'done')]);
    renderWithProviders(<TimelinePage />);
    const completed = await screen.findByRole('button', { name: 'Completed 1' });
    expect(screen.getByRole('button', { name: 'In progress 1' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Waiting for you 1' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Unfinished 1' })).toBeInTheDocument();
    fireEvent.click(completed);
    expect(completed).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('link', { name: 'Task c' })).toHaveAttribute('href', '/tasks/c');
    expect(screen.queryByRole('link', { name: 'Task b' })).not.toBeInTheDocument();
    fireEvent.click(completed);
    expect(screen.getByRole('link', { name: 'Task b' })).toBeInTheDocument();
  });

  it('puts waiting employees before stuck, active, and idle, with task links and elapsed time', async () => {
    respond([task('a', 'active', 'in_progress'), { ...task('b', 'waiting', 'needs_human'), updated_at: '2026-10-30T22:00:00Z' }, task('d', 'stuck', 'blocked')]);
    renderWithProviders(<TimelinePage />);
    await screen.findByRole('link', { name: 'Task b' });
    const cards = screen.getAllByTestId('timeline-employee-card');
    expect(cards.map((card) => within(card).getByText(/^(waiting|stuck|active|idle)$/).textContent)).toEqual(['waiting', 'stuck', 'active', 'idle']);
    expect(within(cards[0]).getByRole('link', { name: 'Task b' })).toHaveAttribute('href', '/tasks/b');
    expect(within(cards[0]).getAllByText(/2 hours since last update/)).toHaveLength(2);
  });

  it('keeps the chart collapsed and opens task links with one status legend', async () => {
    respond([task('a', 'active', 'review')]);
    renderWithProviders(<TimelinePage />);
    await screen.findByRole('link', { name: 'Task a' });
    expect(screen.queryByRole('group', { name: 'Work timeline chart' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByText('View time distribution'));
    const chart = await screen.findByRole('group', { name: 'Work timeline chart' });
    expect(within(chart).getByRole('link')).toHaveAttribute('href', '/tasks/a');
    expect(screen.getByText(/not hours worked/)).toBeInTheDocument();
    expect(chart.querySelector('rect[fill="var(--chart-3)"]')).toBeTruthy();
    expect(screen.queryByText('review')).not.toBeInTheDocument();
  });

  it('shows a friendly empty state with zero counts', async () => {
    renderWithProviders(<TimelinePage />);
    expect(await screen.findByText('No work recorded in this window')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Waiting for you 0' })).toBeInTheDocument();
    expect(screen.queryByText('View time distribution')).not.toBeInTheDocument();
  });

  it('shows a load error with retry', async () => {
    mockWsClient.call.mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<TimelinePage />);
    expect(await screen.findByText('Failed to load the timeline')).toBeInTheDocument();
    respond([task('a', 'active', 'pending')]);
    fireEvent.click(screen.getByRole('button', { name: /try again/i }));
    expect(await screen.findByRole('link', { name: 'Task a' })).toBeInTheDocument();
  });

  it('includes problems without double-counting a linked task and excludes the alert lane', async () => {
    respond([
      task('a', 'stuck', 'failed'),
      { ...task('failure-a', 'stuck', 'error'), kind: 'problem', task_id: 'a' },
      { ...task('failure-b', 'stuck', 'error'), kind: 'problem', task_id: null },
      task('hidden', 'channel_alert', 'failed'),
    ]);
    renderWithProviders(<TimelinePage />);
    expect(await screen.findByRole('button', { name: 'Unfinished 2' })).toBeInTheDocument();
    expect(screen.queryByText('Task hidden')).not.toBeInTheDocument();
    expect(screen.queryByText('error')).not.toBeInTheDocument();
  });

  it('shows all supported store statuses as words', async () => {
    respond(['pending', 'pending_approval', 'review', 'revising'].map((status) => task(status, 'active', status)));
    renderWithProviders(<TimelinePage />);
    expect(await screen.findByRole('button', { name: 'Waiting for you 1' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'In progress 3' })).toBeInTheDocument();
    for (const status of ['pending', 'pending_approval', 'review', 'revising']) expect(screen.queryByText(status, { exact: true })).not.toBeInTheDocument();
  });
});

it('keeps a readable task link when privacy removes an activity title', async () => {
  respond([{ ...task('event', 'stuck', 'error'), kind: 'problem', task_id: 'private-task', label: '' }]);
  renderWithProviders(<TimelinePage />);
  expect(await screen.findByRole('link', { name: 'Needs attention' })).toHaveAttribute('href', '/tasks/private-task');
});
