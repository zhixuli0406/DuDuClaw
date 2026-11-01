import { act } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useGrowthStore } from '@/stores/growth-store';
import type { GrowthSnapshot } from '@/lib/api-growth';

const query = vi.hoisted(() => vi.fn());
vi.mock('@/hooks/useSharedLeaderQuery', () => ({ useSharedLeaderQuery: query }));
vi.mock('./DailyReportCard', () => ({ DailyReportCard: () => null }));
import { GrowthMount } from './GrowthMount';

const summary = { xp: 450, level: 2, xp_into_level: 50, xp_for_next_level: 500 };
const full: GrowthSnapshot = {
  ...summary,
  facts: { agents_count: 7, tasks_completed: 5, knowledge_pages: 4, skills_acquired: 3, routines_completed: 2, custom_skills_approved: 1 },
  achievements: [],
};

beforeEach(() => {
  query.mockReset();
  useAuthStore.setState({ user: { id: 'manager', role: 'manager', status: 'active' } as never });
  useConnectionStore.setState({ state: 'authenticated' as never });
  useGrowthStore.setState({ snapshot: null, loaded: false, error: null });
});

describe('GrowthMount authorization cache', () => {
  it('partitions the poll and clears Manager facts on account/role changes and disconnect', () => {
    query.mockReturnValue({ data: full, error: undefined, refetch: vi.fn() });
    renderWithProviders(<GrowthMount />);
    const managerKey = query.mock.lastCall?.[0];
    expect(useGrowthStore.getState().snapshot?.facts).toBeDefined();

    query.mockReturnValue({ data: undefined, error: undefined, refetch: vi.fn() });
    act(() => useAuthStore.setState({ user: { id: 'manager', role: 'employee', status: 'active' } as never }));
    const employeeKey = query.mock.lastCall?.[0];
    expect(employeeKey).not.toBe(managerKey);
    expect(useGrowthStore.getState().snapshot).toBeNull();

    query.mockReturnValue({ data: summary, error: undefined, refetch: vi.fn() });
    act(() => useAuthStore.setState({ user: { id: 'other', role: 'employee', status: 'active' } as never }));
    expect(query.mock.lastCall?.[0]).not.toBe(employeeKey);
    expect(useGrowthStore.getState().snapshot).toEqual(summary);
    act(() => useConnectionStore.setState({ state: 'disconnected' as never }));
    expect(query.mock.lastCall?.[3]).toBe(false);
    expect(useGrowthStore.getState().snapshot).toBeNull();
  });

  it('clears cached company details when a poll is refused', () => {
    query.mockReturnValue({ data: full, error: undefined, refetch: vi.fn() });
    const { rerender } = renderWithProviders(<GrowthMount />);
    query.mockReturnValue({ data: full, error: new Error('permission denied'), refetch: vi.fn() });
    rerender(<GrowthMount />);
    expect(useGrowthStore.getState().snapshot).toBeNull();
    expect(useGrowthStore.getState().loaded).toBe(false);
  });
});
