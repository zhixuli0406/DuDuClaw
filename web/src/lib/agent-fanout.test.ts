import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture calls to the WS client without a live socket.
const call = vi.fn();
vi.mock('./ws-client', () => ({
  client: { call: (...args: unknown[]) => call(...args), disconnect: vi.fn() },
}));
vi.mock('@/lib/ws-client', () => ({
  client: { call: (...args: unknown[]) => call(...args), disconnect: vi.fn() },
}));

import { api, type TaskInfo } from './api';
import { useAuthStore, type AgentBinding, type UserRole } from '@/stores/auth-store';

function loginAs(role: UserRole, agents: string[]) {
  const bindings: AgentBinding[] = agents.map((agent_name) => ({
    user_id: 'u1',
    agent_name,
    access_level: 'operator',
    bound_at: '2026-09-01T00:00:00Z',
  }));
  useAuthStore.setState({
    user: { id: 'u1', email: 'u@x', display_name: 'U', role, status: 'active' },
    bindings,
    isAuthenticated: true,
  });
}

function task(id: string, agent: string, updated_at: string, pinned = false): TaskInfo {
  return {
    id,
    title: id,
    description: '',
    status: 'needs_human',
    priority: 'medium',
    assigned_to: agent,
    created_by: 'x',
    created_at: updated_at,
    updated_at,
    pinned,
  } as TaskInfo;
}

beforeEach(() => {
  call.mockReset();
  useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
});

describe('tasks.list agent fan-out (non-admin agent_id contract)', () => {
  it('admin → exactly one request with unchanged params', async () => {
    loginAs('admin', ['a', 'b']);
    call.mockResolvedValue({ tasks: [task('t1', 'a', '2026-09-01T00:00:00Z')] });
    const res = await api.tasks.list({ status: 'needs_human' });
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.list', { status: 'needs_human' });
    expect(res.tasks.map((t) => t.id)).toEqual(['t1']);
  });

  it('admin without filters → one request with {}', async () => {
    loginAs('admin', []);
    call.mockResolvedValue({ tasks: [] });
    await api.tasks.list();
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.list', {});
  });

  it('unknown viewer (no user loaded) → one unchanged request', async () => {
    call.mockResolvedValue({ tasks: [] });
    await api.tasks.list({ status: 'blocked' });
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.list', { status: 'blocked' });
  });

  it('explicit agent_id → one unchanged request even for a non-admin', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockResolvedValue({ tasks: [] });
    await api.tasks.list({ status: 'blocked', agent_id: 'a' });
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.list', { status: 'blocked', agent_id: 'a' });
  });

  it('non-admin without agent_id → one request per accessible agent, merged + de-duplicated', async () => {
    loginAs('manager', ['a', 'b', 'a']);
    call.mockImplementation((_m: string, p: { agent_id: string }) => {
      if (p.agent_id === 'a') {
        return Promise.resolve({
          tasks: [task('t1', 'a', '2026-09-01T00:00:00Z'), task('shared', 'a', '2026-09-02T00:00:00Z')],
        });
      }
      return Promise.resolve({
        tasks: [
          task('t2', 'b', '2026-09-03T00:00:00Z'),
          task('shared', 'b', '2026-09-02T00:00:00Z'),
          task('pin', 'b', '2026-08-01T00:00:00Z', true),
        ],
      });
    });
    const res = await api.tasks.list({ status: 'needs_human' });
    expect(call).toHaveBeenCalledTimes(2);
    expect(call).toHaveBeenCalledWith('tasks.list', { status: 'needs_human', agent_id: 'a' });
    expect(call).toHaveBeenCalledWith('tasks.list', { status: 'needs_human', agent_id: 'b' });
    // Same order a single call returns: pinned first, then updated_at DESC.
    expect(res.tasks.map((t) => t.id)).toEqual(['pin', 't2', 'shared', 't1']);
  });

  it('non-admin with zero accessible agents → empty list, no request', async () => {
    loginAs('employee', []);
    const res = await api.tasks.list({ status: 'needs_human' });
    expect(call).not.toHaveBeenCalled();
    expect(res).toEqual({ tasks: [] });
  });

  it('one agent failing still returns the others (partial result)', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockImplementation((_m: string, p: { agent_id: string }) =>
      p.agent_id === 'a'
        ? Promise.reject(new Error('permission denied'))
        : Promise.resolve({ tasks: [task('t2', 'b', '2026-09-03T00:00:00Z')] }),
    );
    const res = await api.tasks.list({ status: 'blocked' });
    expect(res.tasks.map((t) => t.id)).toEqual(['t2']);
    // The failure stays observable on the merged result.
    expect((res.partial_error as Error).message).toBe('permission denied');
  });

  it('a full success carries no partial_error', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockResolvedValue({ tasks: [] });
    const res = await api.tasks.list({ status: 'blocked' });
    expect(res).toEqual({ tasks: [] });
    expect('partial_error' in res).toBe(false);
  });

  it('caps in-flight requests at 6 and still requests every agent', async () => {
    const agents = Array.from({ length: 20 }, (_, i) => `agent-${i}`);
    loginAs('manager', agents);
    let inFlight = 0;
    let peak = 0;
    call.mockImplementation(async (_m: string, p: { agent_id: string }) => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      await new Promise((r) => setTimeout(r, 1));
      inFlight -= 1;
      return { tasks: [task(`t-${p.agent_id}`, p.agent_id, '2026-09-01T00:00:00Z')] };
    });
    const res = await api.tasks.list({ status: 'blocked' });
    expect(peak).toBeLessThanOrEqual(6);
    expect(peak).toBeGreaterThan(1);
    expect(call).toHaveBeenCalledTimes(20);
    expect(new Set(call.mock.calls.map((c) => (c[1] as { agent_id: string }).agent_id))).toEqual(new Set(agents));
    expect(res.tasks).toHaveLength(20);
  });

  it('every agent failing → rejects', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockRejectedValue(new Error('network down'));
    await expect(api.tasks.list({ status: 'blocked' })).rejects.toThrow('network down');
  });
});

describe('activity.list / plans.list agent fan-out', () => {
  it('activity.list: admin → one unchanged request', async () => {
    loginAs('admin', ['a']);
    call.mockResolvedValue({ events: [], total: 0 });
    await api.activity.list({ limit: 50 });
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('activity.list', { limit: 50 });
  });

  it('activity.list: non-admin → per-agent, newest first, capped at limit, totals summed', async () => {
    loginAs('manager', ['a', 'b']);
    call.mockImplementation((_m: string, p: { agent_id: string }) =>
      Promise.resolve(
        p.agent_id === 'a'
          ? {
              events: [
                { id: 'e3', type: 'x', agent_id: 'a', summary: '', timestamp: '2026-09-03T00:00:00Z' },
                { id: 'e1', type: 'x', agent_id: 'a', summary: '', timestamp: '2026-09-01T00:00:00Z' },
              ],
              total: 7,
            }
          : {
              events: [{ id: 'e2', type: 'x', agent_id: 'b', summary: '', timestamp: '2026-09-02T00:00:00Z' }],
              total: 1,
            },
      ),
    );
    const res = await api.activity.list({ limit: 2 });
    expect(call).toHaveBeenCalledWith('activity.list', { limit: 2, agent_id: 'a' });
    expect(call).toHaveBeenCalledWith('activity.list', { limit: 2, agent_id: 'b' });
    expect(res.events.map((e) => e.id)).toEqual(['e3', 'e2']);
    expect(res.total).toBe(8);
    expect(res.partial_error).toBeUndefined();
  });

  it('plans.list: non-admin → per-agent, merged by updated_at DESC', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockImplementation((_m: string, p: { agent_id: string }) =>
      Promise.resolve({
        plans: [{ id: `p-${p.agent_id}`, agent_id: p.agent_id, updated_at: p.agent_id === 'a' ? '2026-09-01T00:00:00Z' : '2026-09-05T00:00:00Z' }],
      }),
    );
    const res = await api.plans.list();
    expect(call).toHaveBeenCalledTimes(2);
    expect(res.plans.map((p) => p.id)).toEqual(['p-b', 'p-a']);
  });

  it('plans.list / activity.list surface a partial failure too', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockImplementation((m: string, p: { agent_id: string }) =>
      p.agent_id === 'a'
        ? Promise.reject(new Error('boom'))
        : Promise.resolve(m === 'plans.list' ? { plans: [] } : { events: [], total: 0 }),
    );
    expect(((await api.plans.list()).partial_error as Error).message).toBe('boom');
    expect(((await api.activity.list()).partial_error as Error).message).toBe('boom');
  });

  it('plans.list: zero accessible agents → empty, no request', async () => {
    loginAs('employee', []);
    const res = await api.plans.list({ status: 'active' as never });
    expect(call).not.toHaveBeenCalled();
    expect(res).toEqual({ plans: [] });
  });
});

describe('tasks.flowMetrics agent fan-out (L2 round 3)', () => {
  const top = { review_queue_depth: 7, review_wip_limit: 5, accepts_last_7d: 14, avg_daily_accepts_7d: 2 };
  const row = (agent_id: string) => ({
    agent_id,
    goal_tasks: 1,
    finished: 1,
    first_pass_yield: 1,
    avg_rounds: 1,
    avg_agent_seconds: 1,
    avg_cycle_seconds: 1,
    review_queue_depth: 1,
  });

  it('admin → one request with {} (unchanged)', async () => {
    loginAs('admin', ['a']);
    call.mockResolvedValue({ agents: [], ...top });
    await api.tasks.flowMetrics();
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.flow_metrics', {});
  });

  it('explicit agent → one request with that agent (unchanged)', async () => {
    loginAs('employee', ['a', 'b']);
    call.mockResolvedValue({ agents: [], ...top });
    await api.tasks.flowMetrics('a');
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith('tasks.flow_metrics', { agent_id: 'a' });
  });

  it('non-admin without agent → per-agent rows merged, system-wide fields NOT summed', async () => {
    loginAs('manager', ['a', 'b']);
    call.mockImplementation((_m: string, p: { agent_id: string }) =>
      Promise.resolve({ agents: [row(p.agent_id), row('a')], ...top }),
    );
    const res = await api.tasks.flowMetrics();
    expect(call).toHaveBeenCalledWith('tasks.flow_metrics', { agent_id: 'a' });
    expect(call).toHaveBeenCalledWith('tasks.flow_metrics', { agent_id: 'b' });
    expect(res?.agents.map((r) => r.agent_id)).toEqual(['a', 'b']);
    expect(res?.review_queue_depth).toBe(7);
    expect(res?.review_wip_limit).toBe(5);
    expect(res?.accepts_last_7d).toBe(14);
    expect(res?.avg_daily_accepts_7d).toBe(2);
    expect(res?.partial_error).toBeUndefined();
  });

  it('non-admin with zero agents → null (board renders without a gauge), no request', async () => {
    loginAs('employee', []);
    expect(await api.tasks.flowMetrics()).toBeNull();
    expect(call).not.toHaveBeenCalled();
  });

  it('partial failure → merged result + partial_error', async () => {
    loginAs('manager', ['a', 'b']);
    call.mockImplementation((_m: string, p: { agent_id: string }) =>
      p.agent_id === 'a' ? Promise.reject(new Error('x')) : Promise.resolve({ agents: [row('b')], ...top }),
    );
    const res = await api.tasks.flowMetrics();
    expect(res?.agents.map((r) => r.agent_id)).toEqual(['b']);
    expect(res?.review_wip_limit).toBe(5);
    expect((res?.partial_error as Error).message).toBe('x');
  });
});
