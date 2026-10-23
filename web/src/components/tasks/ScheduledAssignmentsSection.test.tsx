import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import type { AssignScheduleInfo } from '@/lib/api';
import { ScheduledAssignmentsSection } from './ScheduledAssignmentsSection';

const row = (over: Partial<AssignScheduleInfo>): AssignScheduleInfo => ({
  id: 'sched-1',
  agent_id: 'sales-rep',
  kind: 'goal',
  title: 'Weekly report',
  description: 'Weekly report',
  schedule: { kind: 'weekly', days: [1, 3], hour: 9, minute: 0 },
  timezone: 'Asia/Taipei',
  status: 'active',
  pause_reason: null,
  created_via: 'dashboard',
  creator_user_id: 'u1',
  creator_name: null,
  next_fire_at: '2026-10-26T01:00:00+00:00',
  last_fire_at: null,
  last_task_id: null,
  last_outcome: null,
  fire_count: 0,
  skip_count: 0,
  created_at: '2026-10-23T02:00:00+00:00',
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
});

describe('ScheduledAssignmentsSection', () => {
  it('keeps narrowing controls when scheduling limits need an administrator to fix them', async () => {
    mockWsClient.call.mockResolvedValue({
      schedules: [row({}), row({ id: 'paused', status: 'paused', pause_reason: 'by_user' })],
      max_active: null, min_interval_minutes: null,
    });
    renderWithProviders(<ScheduledAssignmentsSection enabled isAdmin={false} />);
    expect(await screen.findByText(/^Scheduling settings need attention/)).toHaveAttribute('role', 'status');
    expect(screen.getByRole('button', { name: 'Pause' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Resume' })).toBeDisabled();
    expect(screen.getAllByRole('button', { name: 'Remove schedule' })[0]).toBeEnabled();
  });

  it('refreshes when the shared assign panel creates a schedule', async () => {
    mockWsClient.call.mockResolvedValue({ schedules: [] });
    renderWithProviders(<ScheduledAssignmentsSection enabled isAdmin={false} />);
    await screen.findByText(/No schedules yet/);
    const subscription = mockWsClient.subscribe.mock.calls.find(([event]) => event === 'assign_schedule.changed');
    expect(subscription).toBeDefined();
    mockWsClient.call.mockResolvedValue({ schedules: [row({})] });
    act(() => subscription![1]({}));
    expect(await screen.findByText('Weekly report')).toBeInTheDocument();
  });

  it('lists schedules, explains an automatic pause and pauses / resumes / removes', async () => {
    mockWsClient.call.mockImplementation((method: string) =>
      Promise.resolve(
        method === 'assign_schedules.list'
          ? {
              schedules: [
                row({}),
                row({ id: 'sched-2', title: 'Lost one', status: 'paused', pause_reason: 'access_lost' }),
              ],
              max_active: 20,
              min_interval_minutes: 60,
            }
          : { schedule: row({}) },
      ),
    );
    renderWithProviders(<ScheduledAssignmentsSection enabled isAdmin={false} />);
    expect(await screen.findByText('Weekly report')).toBeInTheDocument();
    expect(screen.getAllByText('Every Mon, Wed at 09:00')).toHaveLength(2);
    expect(screen.getByText('Paused: creator lost permission')).toBeInTheDocument();
    expect(screen.queryByText("Show everyone's")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Pause' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.pause', { id: 'sched-1' }));
    fireEvent.click(screen.getByRole('button', { name: 'Resume' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.resume', { id: 'sched-2' }));

    // Removing asks once more before it acts.
    const [remove] = screen.getAllByRole('button', { name: 'Remove schedule' });
    fireEvent.click(remove);
    expect(mockWsClient.call).not.toHaveBeenCalledWith('assign_schedules.remove', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: 'Remove?' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.remove', { id: 'sched-1' }));
  });

  it("lets an admin switch to everyone's schedules", async () => {
    mockWsClient.call.mockResolvedValue({ schedules: [], max_active: 20, min_interval_minutes: 5 });
    renderWithProviders(<ScheduledAssignmentsSection enabled isAdmin />);
    expect(await screen.findByText(/No schedules yet/)).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.list', { scope: 'mine' });
    fireEvent.click(screen.getByRole('switch'));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.list', { scope: 'all' }));
  });

  it('renders nothing when the view is not "assigned by me"', () => {
    const { container } = renderWithProviders(<ScheduledAssignmentsSection enabled={false} isAdmin={false} />);
    expect(container).toBeEmptyDOMElement();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });
});
