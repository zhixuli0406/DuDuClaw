import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

const { handlers, call } = vi.hoisted(() => ({
  handlers: new Map<string, Set<(payload: unknown) => void>>(),
  call: vi.fn(),
}));
vi.mock('@/lib/ws-client', () => ({
  client: {
    call,
    subscribe: (event: string, callback: (payload: unknown) => void) => {
      const subscribers = handlers.get(event) ?? new Set<(payload: unknown) => void>();
      subscribers.add(callback);
      handlers.set(event, subscribers);
      return () => subscribers.delete(callback);
    },
  },
}));

let useApprovalsStore: typeof import('./approvals-store').useApprovalsStore;
let useTasksStore: typeof import('./tasks-store').useTasksStore;
let useAuthStore: typeof import('./auth-store').useAuthStore;
let useAgentsStore: typeof import('./agents-store').useAgentsStore;

function emit(event: string, payload: unknown = {}) {
  for (const callback of handlers.get(event) ?? []) callback(payload);
}

beforeEach(async () => {
  vi.useFakeTimers();
  vi.resetModules();
  handlers.clear();
  ({ useApprovalsStore } = await import('./approvals-store'));
  ({ useTasksStore } = await import('./tasks-store'));
  ({ useAuthStore } = await import('./auth-store'));
  ({ useAgentsStore } = await import('./agents-store'));
  call.mockReset();
  call.mockImplementation((method: string) => {
    if (method === 'approvals.list') return Promise.resolve({ approvals: [], count: 2 });
    if (method === 'budget.incidents') return Promise.resolve({ incidents: [] });
    if (method === 'tasks.list') return Promise.resolve({ tasks: [] });
    return Promise.resolve(null);
  });
  useAuthStore.setState({
    user: { id: 'admin', email: 'a@x', display_name: 'Admin', role: 'admin', status: 'active' },
    isAuthenticated: true,
  });
  useApprovalsStore.setState({ pendingCount: 7, stale: false });
  useTasksStore.setState({ tasks: [], error: null });
});
afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('live event invalidation', () => {
  it('refreshes the roster on turn events without needing session or task ids', async () => {
    call.mockImplementation((method: string) => Promise.resolve(
      method === 'agents.list' ? { agents: [{ name: 'employee', status: 'active' }] } : {},
    ));
    emit('agent.turn_started', { agent_id: 'employee', source: 'channel', runtime: 'claude' });
    await vi.advanceTimersByTimeAsync(0);
    expect(useAgentsStore.getState().agents).toHaveLength(1);
    emit('agent.turn_finished', { agent_id: 'employee', source: 'channel', runtime: 'claude' });
    await vi.advanceTimersByTimeAsync(1_999);
    expect(call.mock.calls.filter(([method]) => method === 'agents.list')).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(call.mock.calls.filter(([method]) => method === 'agents.list')).toHaveLength(2);
  });

  it('bounds each store during a continuous event flood and eventually reads the tail', async () => {
    for (let i = 0; i < 50; i += 1) {
      emit('task.updated', { id: `forged-${i}` });
      emit('agent.status_changed', { agent_id: `forged-${i}` });
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(call.mock.calls.filter(([method]) => method === 'approvals.list')).toHaveLength(3);
    expect(call.mock.calls.filter(([method]) => method === 'agents.list')).toHaveLength(3);
    expect(call.mock.calls.filter(([method, params]) => method === 'tasks.list' && !params.status)).toHaveLength(3);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(call.mock.calls.filter(([method]) => method === 'approvals.list')).toHaveLength(4);
    expect(call.mock.calls.filter(([method]) => method === 'agents.list')).toHaveLength(4);
    expect(useApprovalsStore.getState().stale).toBe(false);
  });

  it('backs off partial inbox reads even when a new event invalidated the response', async () => {
    let finish!: (value: { tasks: never[]; partial_error: string }) => void;
    call.mockImplementation((method: string, params: { status?: string }) => {
      if (method === 'tasks.list' && params.status === 'blocked') {
        return new Promise((resolve) => { finish = resolve; });
      }
      return Promise.resolve({ tasks: [], incidents: [], approvals: [], count: 2 });
    });
    const count = () => call.mock.calls.filter(([method]) => method === 'approvals.list').length;
    emit('approval.created');
    await vi.advanceTimersByTimeAsync(0);
    emit('approval.decided');
    finish({ tasks: [], partial_error: 'unavailable employee' });
    await vi.advanceTimersByTimeAsync(2_000);
    expect(count()).toBe(2);
    emit('approval.decided');
    finish({ tasks: [], partial_error: 'unavailable employee' });
    await vi.advanceTimersByTimeAsync(3_999);
    expect(count()).toBe(2);
    expect(useApprovalsStore.getState()).toMatchObject({ pendingCount: 7, stale: true });
    await vi.advanceTimersByTimeAsync(1);
    expect(count()).toBe(3);
    finish({ tasks: [], partial_error: 'unavailable employee' });
    await vi.advanceTimersByTimeAsync(0);
  });

  it('backs off failed inbox, task and roster reads, then resets on recovery', async () => {
    call.mockRejectedValue(new Error('offline'));
    const invalidateAll = () => {
      emit('task.updated');
      emit('agent.status_changed');
    };
    const count = (method: string) => call.mock.calls.filter(([name]) => name === method).length;
    invalidateAll();
    await vi.advanceTimersByTimeAsync(0);
    invalidateAll();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(count('approvals.list')).toBe(2);
    expect(count('agents.list')).toBe(2);
    invalidateAll();
    await vi.advanceTimersByTimeAsync(3_999);
    expect(count('approvals.list')).toBe(2);
    expect(count('agents.list')).toBe(2);
    expect(count('tasks.list')).toBe(6);
    call.mockResolvedValue({ tasks: [], agents: [], approvals: [], incidents: [] });
    await vi.advanceTimersByTimeAsync(1);
    expect(count('approvals.list')).toBe(3);
    expect(count('agents.list')).toBe(3);
    invalidateAll();
    await vi.advanceTimersByTimeAsync(1_999);
    expect(count('approvals.list')).toBe(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(count('approvals.list')).toBe(4);
    expect(count('agents.list')).toBe(4);
    expect(useApprovalsStore.getState().stale).toBe(false);
  });

  it.each(['approval.created', 'approval.decided', 'approval.expired', 'task.needs_human', 'budget.breaker_open'])(
    '%s refreshes the inbox count', async (event) => {
      emit(event);
      expect(useApprovalsStore.getState().stale).toBe(true);
      await vi.advanceTimersByTimeAsync(100);
      expect(call).toHaveBeenCalledWith('approvals.list', {});
      expect(useApprovalsStore.getState()).toMatchObject({ pendingCount: 2, stale: false });
    },
  );

  it('coalesces duplicate direct and database pushes without inserting partial task rows', async () => {
    emit('task.created', { id: 't1', agent_id: 'employee' });
    emit('task.created', { id: 't1', agent_id: 'employee' });
    emit('goal.round_finished', { task_id: 't1' });
    expect(useTasksStore.getState().tasks).toEqual([]);
    await vi.advanceTimersByTimeAsync(100);
    expect(call.mock.calls.filter(([method]) => method === 'approvals.list')).toHaveLength(1);
    // One board read and the two distinct inbox task statuses.
    expect(call.mock.calls.filter(([method]) => method === 'tasks.list')).toHaveLength(3);
  });

  it.each(['events_lost', 'connection.ready'])('%s reconciles tasks and approvals', async (event) => {
    emit(event);
    await vi.advanceTimersByTimeAsync(100);
    expect(call.mock.calls.some(([method]) => method === 'tasks.list')).toBe(true);
    expect(call.mock.calls.some(([method]) => method === 'approvals.list')).toBe(true);
  });

  it('reconciles the task board and roster on loss with retained filters and a shared two-second floor', async () => {
    call.mockImplementation((method: string) => Promise.resolve(
      method === 'agents.list'
        ? { agents: [{ name: 'recovered', status: 'active' }] }
        : method === 'tasks.list'
          ? { tasks: [{ id: 'recovered-task' }] }
          : { approvals: [], count: 0, incidents: [] },
    ));
    await useTasksStore.getState().fetchTasks({ agent_id: 'employee' });
    await useAgentsStore.getState().fetchAgents(true);
    call.mockClear();
    useTasksStore.setState({ tasks: [] });
    useAgentsStore.setState({ agents: [] });
    const boardReads = () => call.mock.calls.filter(([method, params]) =>
      method === 'tasks.list' && params.agent_id === 'employee' && !params.status).length;
    const rosterReads = () => call.mock.calls.filter(([method]) => method === 'agents.list').length;
    emit('events_lost');
    await vi.advanceTimersByTimeAsync(0);
    expect(boardReads()).toBe(1);
    expect(rosterReads()).toBe(1);
    expect(useTasksStore.getState().tasks).toEqual([{ id: 'recovered-task' }]);
    expect(useAgentsStore.getState().agents).toEqual([{ name: 'recovered', status: 'active' }]);
    expect(call).toHaveBeenCalledWith('agents.list', { include_archived: true });
    // Loss and ordinary invalidations must share one refresh limiter.
    emit('events_lost');
    emit('task.updated');
    emit('agent.status_changed');
    await vi.advanceTimersByTimeAsync(1_999);
    expect(boardReads()).toBe(1);
    expect(rosterReads()).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(boardReads()).toBe(2);
    expect(rosterReads()).toBe(2);
  });

  it('refetches again when an invalidation arrives during a read', async () => {
    let finish!: (value: { count: number; approvals: never[] }) => void;
    let reads = 0;
    call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        reads += 1;
        if (reads === 1) return new Promise((resolve) => { finish = resolve; });
        return Promise.resolve({ approvals: [], count: 4 });
      }
      return Promise.resolve({ tasks: [], incidents: [] });
    });
    emit('approval.created');
    await vi.advanceTimersByTimeAsync(100);
    emit('approval.decided');
    finish({ count: 1, approvals: [] });
    await vi.advanceTimersByTimeAsync(100);
    expect(reads).toBe(1);
    expect(useApprovalsStore.getState().stale).toBe(true);
    await vi.advanceTimersByTimeAsync(1_800);
    expect(reads).toBe(2);
    expect(useApprovalsStore.getState()).toMatchObject({ pendingCount: 4, stale: false });
  });

  it('keeps the last count and stale marker when reconciliation fails', async () => {
    call.mockRejectedValue(new Error('offline'));
    emit('events_lost');
    await vi.advanceTimersByTimeAsync(100);
    expect(useApprovalsStore.getState()).toMatchObject({ pendingCount: 7, stale: true });
  });

  it('an older in-flight response cannot clear a newer invalidation', async () => {
    let finish!: (value: { count: number; approvals: never[] }) => void;
    call.mockImplementation((method: string) => {
      if (method === 'approvals.list') return new Promise((resolve) => { finish = resolve; });
      return Promise.resolve({ tasks: [], incidents: [] });
    });
    const read = useApprovalsStore.getState().fetchCount();
    emit('connection.stale');
    finish({ count: 0, approvals: [] });
    await read;
    expect(useApprovalsStore.getState()).toMatchObject({ pendingCount: 7, stale: true });
  });
});
