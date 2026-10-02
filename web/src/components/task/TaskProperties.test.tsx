import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
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
