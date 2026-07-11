import { describe, it, expect, vi, afterEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { pickLeader, LeaderCore, useSharedLeaderQuery } from './useSharedLeaderQuery';

type LeaderMsg = Parameters<LeaderCore['receive']>[0];

/** Synchronous in-memory bus: delivers a post to every other core at once. */
function connect(cores: LeaderCore[]) {
  return (selfIndex: number) => (msg: LeaderMsg) => {
    for (let i = 0; i < cores.length; i += 1) {
      if (i !== selfIndex) cores[i].receive(msg);
    }
  };
}

describe('pickLeader (election core)', () => {
  it('a lone tab always leads (single-tab fallback)', () => {
    expect(pickLeader('anything', [])).toBe(true);
  });

  it('the lexicographically smallest live id leads, order-independently', () => {
    expect(pickLeader('bbb', ['aaa', 'ccc'])).toBe(false); // aaa is smaller
    expect(pickLeader('aaa', ['bbb', 'ccc'])).toBe(true);
    expect(pickLeader('aaa', ['ccc', 'bbb'])).toBe(true);
    expect(pickLeader('mmm', ['zzz'])).toBe(true);
  });
});

describe('LeaderCore election across two tabs', () => {
  it('elects exactly one leader — the smaller id — after both announce', () => {
    const cores: LeaderCore[] = [];
    const post = connect(cores);
    const a = new LeaderCore({ selfId: 'aaaa', post: post(0) });
    cores.push(a);
    const b = new LeaderCore({ selfId: 'bbbb', post: post(1) });
    cores.push(b);

    a.announce();
    b.announce();

    expect(a.isLeader()).toBe(true);
    expect(b.isLeader()).toBe(false);
    // Exactly one leader.
    expect([a, b].filter((c) => c.isLeader())).toHaveLength(1);
  });

  it('hands leadership off when the current leader leaves', () => {
    const cores: LeaderCore[] = [];
    const post = connect(cores);
    const a = new LeaderCore({ selfId: 'aaaa', post: post(0) });
    cores.push(a);
    const b = new LeaderCore({ selfId: 'bbbb', post: post(1) });
    cores.push(b);
    a.announce();
    b.announce();
    expect(b.isLeader()).toBe(false);

    // Leader tab closes → its `bye` promotes the survivor.
    a.leave();
    expect(b.isLeader()).toBe(true);
  });

  it('relays data from leader to follower', () => {
    const cores: LeaderCore[] = [];
    const post = connect(cores);
    const a = new LeaderCore({ selfId: 'aaaa', post: post(0) });
    cores.push(a);
    const b = new LeaderCore({ selfId: 'bbbb', post: post(1) });
    cores.push(b);
    a.announce();
    b.announce();

    const seen: unknown[] = [];
    b.onData = (payload) => seen.push(payload);
    a.publishData({ ok: 1 });
    expect(seen).toEqual([{ ok: 1 }]);
  });
});

describe('useSharedLeaderQuery — single-tab fallback (no BroadcastChannel)', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('polls locally as its own leader when BroadcastChannel is absent', async () => {
    vi.stubGlobal('BroadcastChannel', undefined);
    const queryFn = vi.fn(() => 'PONG');

    const { result, unmount } = renderHook(() =>
      useSharedLeaderQuery('fallback-key-1', queryFn, 100_000),
    );

    // Initial poll fires immediately for a lone leader; flush the resolve.
    await act(async () => {
      await Promise.resolve();
    });

    await waitFor(() => expect(result.current.data).toBe('PONG'));
    expect(result.current.isLeader).toBe(true);
    expect(result.current.loading).toBe(false);
    expect(queryFn).toHaveBeenCalled();

    unmount();
  });

  it('detaches entirely when disabled', () => {
    vi.stubGlobal('BroadcastChannel', undefined);
    const queryFn = vi.fn(() => 'X');

    const { result, unmount } = renderHook(() =>
      useSharedLeaderQuery('fallback-key-2', queryFn, 100_000, /* enabled */ false),
    );

    expect(queryFn).not.toHaveBeenCalled();
    expect(result.current.isLeader).toBe(true); // default when detached
    expect(result.current.loading).toBe(false);
    unmount();
  });
});
