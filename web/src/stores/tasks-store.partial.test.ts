import { describe, it, expect, beforeEach } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useTasksStore } from './tasks-store';
import { useAuthStore } from './auth-store';

// L2 round 3: a fanned-out list that came back partial keeps its rows and
// records the failure; the next fully successful load clears it.
beforeEach(() => {
  mockWsClient.call.mockReset();
  useTasksStore.setState({ tasks: [], activities: [], error: null, tasksPartialError: null, activitiesPartialError: null });
  useAuthStore.setState({
    user: { id: 'u1', email: 'u@x', display_name: 'U', role: 'employee', status: 'active' },
    bindings: ['a', 'b'].map((agent_name) => ({ user_id: 'u1', agent_name, access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' })),
    isAuthenticated: true,
  });
});

const fail = (agent: string) => (method: string, params?: Record<string, unknown>) => {
  if (params?.agent_id === agent) return Promise.reject(new Error('boom'));
  if (method === 'activity.list') {
    return Promise.resolve({ events: [{ id: `e-${params?.agent_id}`, type: 'task_created', agent_id: params?.agent_id, summary: '', timestamp: '2026-09-01T00:00:00Z' }], total: 1 });
  }
  return Promise.resolve({ tasks: [] });
};

describe('tasks-store partial fan-out signal', () => {
  it('fetchActivities: partial → rows kept + activitiesPartialError; clean → cleared', async () => {
    mockWsClient.call.mockImplementation(fail('a'));
    await useTasksStore.getState().fetchActivities({ limit: 10 });
    expect(useTasksStore.getState().activities.map((e) => e.id)).toEqual(['e-b']);
    expect(useTasksStore.getState().activitiesPartialError).toBeInstanceOf(Error);
    expect(useTasksStore.getState().error).toBeNull();

    mockWsClient.call.mockImplementation(fail('nobody'));
    await useTasksStore.getState().fetchActivities({ limit: 10 });
    expect(useTasksStore.getState().activitiesPartialError).toBeNull();
  });

  it('fetchTasks: partial → tasksPartialError; clean → cleared', async () => {
    mockWsClient.call.mockImplementation(fail('a'));
    await useTasksStore.getState().fetchTasks();
    expect(useTasksStore.getState().tasksPartialError).toBeInstanceOf(Error);
    expect(useTasksStore.getState().error).toBeNull();
    mockWsClient.call.mockImplementation(fail('nobody'));
    await useTasksStore.getState().fetchTasks();
    expect(useTasksStore.getState().tasksPartialError).toBeNull();
  });
});
