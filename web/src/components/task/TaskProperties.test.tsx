import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { resetCompanyTimeZone, useCompanyTimeStore } from '@/lib/company-time';
import { TaskProperties } from './TaskProperties';
import type { TaskInfo } from '@/lib/api';

const BASE: TaskInfo = {
  id: 't1', title: 'T', description: '', status: 'queued', priority: 'high', assigned_to: 'nova',
  created_by: 'u', created_at: '2026-09-30T00:00:00Z', updated_at: '2026-09-30T00:00:00Z', kind: 'discovery', tags: [],
};
const AGENTS = [{ name: 'nova', display_name: 'Nova' }];
const noop = { onStatusChange: vi.fn(), onPriorityChange: vi.fn(), onAssign: vi.fn() };

describe('<TaskProperties> lifecycleLocked (L2 round 5)', () => {
  it('discovery: no status / priority / assignee pickers, shows the Goals hint', () => {
    renderWithProviders(<TaskProperties task={BASE} agents={AGENTS} statusLocked lifecycleLocked {...noop} />);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
    expect(screen.getByText(/managed in the exploration section of the Goals page/)).toBeInTheDocument();
    expect(screen.getByText('Nova')).toBeInTheDocument();
  });

  it('ordinary task: pickers stay', () => {
    renderWithProviders(<TaskProperties task={{ ...BASE, kind: 'task', status: 'todo' }} agents={AGENTS} {...noop} />);
    expect(screen.getAllByRole('button').length).toBeGreaterThanOrEqual(3);
    expect(screen.queryByText(/managed in the exploration section/)).toBeNull();
  });
});


describe('<TaskProperties> company time zone', () => {
  beforeEach(() => { resetCompanyTimeZone(); });
  afterEach(() => { resetCompanyTimeZone(); });

  const task = { ...BASE, created_at: '2026-11-27T01:30:00Z', updated_at: '2026-11-27T01:30:00Z' };
  const expectedTime = (zone: string) => new Date(task.created_at).toLocaleString('en', {
    timeZone: zone, year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  });

  it('shows 09:30 in Taipei for a 01:30 UTC timestamp', () => {
    useCompanyTimeStore.setState({ status: 'set', zone: 'Asia/Taipei' });
    renderWithProviders(<TaskProperties task={task} agents={AGENTS} statusLocked lifecycleLocked {...noop} />);
    expect(screen.getAllByText(expectedTime('Asia/Taipei'))).toHaveLength(2);
    expect(screen.getAllByText(/09:30/)).toHaveLength(2);
  });

  it('updates displayed dates when the company zone loads after rendering', () => {
    renderWithProviders(<TaskProperties task={task} agents={AGENTS} statusLocked lifecycleLocked {...noop} />);
    const browserTime = new Date(task.created_at).toLocaleString('en', {
      year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
    });
    expect(screen.getAllByText(browserTime)).toHaveLength(2);
    // UTC also proves the subscription works on browsers already in Taipei.
    act(() => { useCompanyTimeStore.setState({ status: 'set', zone: 'UTC' }); });
    expect(screen.getAllByText(expectedTime('UTC'))).toHaveLength(2);
    act(() => { useCompanyTimeStore.setState({ status: 'set', zone: 'Asia/Taipei' }); });
    expect(screen.getAllByText(expectedTime('Asia/Taipei'))).toHaveLength(2);
  });
});
