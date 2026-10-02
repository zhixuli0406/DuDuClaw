import { describe, it, expect, beforeEach } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useTasksStore } from './tasks-store';
import { usePlansStore } from './plans-store';
import { useAuthStore } from './auth-store';

// L2 round 7: fan-out made un-scoped lists slower, so an older response could
// land after a newer one and overwrite its rows and partial flag.

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

beforeEach(() => {
  mockWsClient.call.mockReset();
  useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  useTasksStore.setState({ tasks: [], activities: [], error: null, tasksPartialError: null, activitiesPartialError: null });
  usePlansStore.setState({ plans: [], error: null, partialError: null });
});

describe('latest-request-wins', () => {
  it('fetchTasks: a slow first response does not overwrite the second', async () => {
    const slow = deferred<unknown>();
    mockWsClient.call
      .mockImplementationOnce(() => slow.promise)
      .mockImplementationOnce(() => Promise.resolve({ tasks: [{ id: 'new' }] }));
    const first = useTasksStore.getState().fetchTasks();
    await useTasksStore.getState().fetchTasks({ agent_id: 'a' });
    slow.resolve({ tasks: [{ id: 'old' }], partial_error: new Error('stale') });
    await first;
    expect(useTasksStore.getState().tasks.map((t) => t.id)).toEqual(['new']);
    expect(useTasksStore.getState().tasksPartialError).toBeNull();
  });

  it('fetchActivities: a slow first response does not overwrite the second', async () => {
    const slow = deferred<unknown>();
    mockWsClient.call
      .mockImplementationOnce(() => slow.promise)
      .mockImplementationOnce(() => Promise.resolve({ events: [{ id: 'new' }], total: 1 }));
    const first = useTasksStore.getState().fetchActivities();
    await useTasksStore.getState().fetchActivities({ agent_id: 'a' });
    slow.resolve({ events: [{ id: 'old' }], total: 1, partial_error: new Error('stale') });
    await first;
    expect(useTasksStore.getState().activities.map((e) => e.id)).toEqual(['new']);
    expect(useTasksStore.getState().activitiesPartialError).toBeNull();
  });

  it('fetchPlans: a slow first response does not overwrite the second', async () => {
    const slow = deferred<unknown>();
    mockWsClient.call
      .mockImplementationOnce(() => slow.promise)
      .mockImplementationOnce(() => Promise.resolve({ plans: [{ id: 'new' }] }));
    const first = usePlansStore.getState().fetchPlans();
    await usePlansStore.getState().fetchPlans({ agent_id: 'a' });
    slow.resolve({ plans: [{ id: 'old' }], partial_error: new Error('stale') });
    await first;
    expect(usePlansStore.getState().plans.map((p) => p.id)).toEqual(['new']);
    expect(usePlansStore.getState().partialError).toBeNull();
  });
});

describe('a total failure resets the partial flag (L2 round 7)', () => {
  it('fetchActivities / fetchTasks / fetchPlans', async () => {
    useTasksStore.setState({ tasksPartialError: new Error('x'), activitiesPartialError: new Error('x') });
    usePlansStore.setState({ partialError: new Error('x') });
    mockWsClient.call.mockRejectedValue(new Error('down'));
    await useTasksStore.getState().fetchActivities();
    await useTasksStore.getState().fetchTasks();
    await usePlansStore.getState().fetchPlans();
    expect(useTasksStore.getState().activitiesPartialError).toBeNull();
    expect(useTasksStore.getState().tasksPartialError).toBeNull();
    expect(usePlansStore.getState().partialError).toBeNull();
  });
});
