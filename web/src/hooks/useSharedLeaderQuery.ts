import { useEffect, useRef, useState } from 'react';

/**
 * useSharedLeaderQuery — multi-tab shared polling (dashboard-redesign-v2 §4.4,
 * paperclip P9 "useSharedPollingQuery"). When the dashboard is open in several
 * tabs, only ONE tab (the elected leader) actually hits the RPC; it broadcasts
 * each result over a `BroadcastChannel`, and every other tab renders that same
 * payload for free. This keeps N tabs from N-multiplying gateway load.
 *
 * Design:
 *  - Election is deterministic and cheap: the tab with the lexicographically
 *    smallest live id leads (`pickLeader`). Presence is tracked with periodic
 *    `ping`s; a peer that goes quiet is pruned and leadership recomputes, so a
 *    closed leader tab is replaced within a few seconds with no flicker.
 *  - When `BroadcastChannel` is unavailable (SSR / very old browsers / jsdom),
 *    the tab is trivially its own leader and polls normally — the single-tab
 *    fallback. No behavioural difference for a lone tab either way.
 *  - Within one tab, all hook instances sharing a `key` share ONE core + timer
 *    (refcounted group), so the same dedup applies to duplicate mounts too.
 *
 * The election core (`pickLeader`, `LeaderCore`) is transport-injectable and
 * exported so it can be unit-tested without real BroadcastChannel/timers.
 */

// ── Wire protocol ──────────────────────────────────────────────────────────
type LeaderMsg =
  | { __sl: true; t: 'hello'; id: string }
  | { __sl: true; t: 'ping'; id: string }
  | { __sl: true; t: 'bye'; id: string }
  | { __sl: true; t: 'poke'; id: string }
  | { __sl: true; t: 'data'; id: string; payload: unknown; ts: number };

function isLeaderMsg(v: unknown): v is LeaderMsg {
  return typeof v === 'object' && v !== null && (v as { __sl?: unknown }).__sl === true;
}

/**
 * Pure leadership decision: `selfId` leads iff no known peer has a smaller id.
 * A lone tab (empty `peerIds`) always leads — this is the single-tab fallback
 * expressed as data. Ids are unique random strings, so exactly one tab leads.
 */
export function pickLeader(selfId: string, peerIds: Iterable<string>): boolean {
  for (const p of peerIds) {
    if (p < selfId) return false;
  }
  return true;
}

export interface LeaderCoreOpts {
  selfId: string;
  /** Send a message to all OTHER cores/tabs on the channel (never to self). */
  post: (msg: LeaderMsg) => void;
  /** Clock injection for deterministic tests. */
  now?: () => number;
  /** A peer un-pinged for longer than this is considered gone. */
  staleMs?: number;
}

/**
 * Transport-agnostic election + data-relay state machine. One per tab per key.
 * Feed it inbound messages via `receive`; it emits leadership changes and
 * relayed data through the two callbacks.
 */
export class LeaderCore {
  readonly selfId: string;
  /** peer id → last-seen epoch ms. */
  private readonly peers = new Map<string, number>();
  private readonly post: (msg: LeaderMsg) => void;
  private readonly now: () => number;
  private readonly staleMs: number;
  private leader: boolean;

  onLeadershipChange?: (isLeader: boolean) => void;
  onData?: (payload: unknown, ts: number) => void;
  onPoke?: () => void;

  constructor(opts: LeaderCoreOpts) {
    this.selfId = opts.selfId;
    this.post = opts.post;
    this.now = opts.now ?? (() => Date.now());
    this.staleMs = opts.staleMs ?? 8000;
    this.leader = true; // lone until a smaller peer is heard
  }

  isLeader(): boolean {
    return this.leader;
  }

  /** Announce arrival so existing tabs reveal themselves (they reply `ping`). */
  announce(): void {
    this.post({ __sl: true, t: 'hello', id: this.selfId });
  }

  /** Periodic keep-alive + prune of vanished peers. */
  heartbeat(): void {
    this.post({ __sl: true, t: 'ping', id: this.selfId });
    this.prune();
  }

  /** Broadcast a fresh query result to followers. */
  publishData(payload: unknown, ts?: number): void {
    this.post({ __sl: true, t: 'data', id: this.selfId, payload, ts: ts ?? this.now() });
  }

  /** Ask the current leader to poll now (used by a follower's refetch). */
  poke(): void {
    this.post({ __sl: true, t: 'poke', id: this.selfId });
  }

  /** Announce departure so leadership hands off immediately. */
  leave(): void {
    this.post({ __sl: true, t: 'bye', id: this.selfId });
  }

  /** Handle one inbound message from another tab. */
  receive(msg: LeaderMsg): void {
    if (msg.id === this.selfId) return; // ignore our own echoes
    switch (msg.t) {
      case 'hello':
        this.touch(msg.id);
        // Reveal ourselves to the newcomer so it can elect correctly.
        this.post({ __sl: true, t: 'ping', id: this.selfId });
        break;
      case 'ping':
        this.touch(msg.id);
        break;
      case 'data':
        this.touch(msg.id);
        this.onData?.(msg.payload, msg.ts);
        break;
      case 'poke':
        this.touch(msg.id);
        if (this.leader) this.onPoke?.();
        break;
      case 'bye':
        this.peers.delete(msg.id);
        this.recompute();
        break;
    }
  }

  private touch(id: string): void {
    this.peers.set(id, this.now());
    this.recompute();
  }

  private prune(): void {
    const cutoff = this.now() - this.staleMs;
    let changed = false;
    for (const [id, ts] of this.peers) {
      if (ts < cutoff) {
        this.peers.delete(id);
        changed = true;
      }
    }
    if (changed) this.recompute();
  }

  private aliveIds(): string[] {
    const cutoff = this.now() - this.staleMs;
    const ids: string[] = [];
    for (const [id, ts] of this.peers) if (ts >= cutoff) ids.push(id);
    return ids;
  }

  private recompute(): void {
    const next = pickLeader(this.selfId, this.aliveIds());
    if (next !== this.leader) {
      this.leader = next;
      this.onLeadershipChange?.(next);
    }
  }
}

// ── Per-tab, per-key shared group (refcounted) ──────────────────────────────
const HEARTBEAT_MS = 3000;

interface Group {
  core: LeaderCore;
  channel: BroadcastChannel | null;
  queryFn: () => Promise<unknown> | unknown;
  intervalMs: number;
  refs: number;
  data: unknown;
  loaded: boolean;
  error: unknown;
  pollTimer: ReturnType<typeof setInterval> | null;
  heartbeatTimer: ReturnType<typeof setInterval> | null;
  subscribers: Set<() => void>;
  destroy: () => void;
  runQuery: () => void;
}

const groups = new Map<string, Group>();

function randomId(): string {
  try {
    if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID();
  } catch {
    /* fall through */
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function ensureGroup(
  key: string,
  queryFn: () => Promise<unknown> | unknown,
  intervalMs: number,
): Group {
  const existing = groups.get(key);
  if (existing) {
    // Keep the query current (contract: one key ⇒ one logical query).
    existing.queryFn = queryFn;
    existing.intervalMs = intervalMs;
    return existing;
  }

  const hasBC = typeof BroadcastChannel !== 'undefined';
  const channel = hasBC ? new BroadcastChannel(`sl:${key}`) : null;
  const selfId = randomId();

  const core = new LeaderCore({
    selfId,
    post: (msg) => channel?.postMessage(msg),
  });

  const notify = () => {
    for (const cb of group.subscribers) cb();
  };

  const group: Group = {
    core,
    channel,
    queryFn,
    intervalMs,
    refs: 0,
    data: undefined,
    loaded: false,
    error: undefined,
    pollTimer: null,
    heartbeatTimer: null,
    subscribers: new Set(),
    runQuery: () => {
      if (!core.isLeader()) return;
      try {
        const out = group.queryFn();
        Promise.resolve(out).then(
          (payload) => {
            group.data = payload;
            group.loaded = true;
            group.error = undefined;
            core.publishData(payload);
            notify();
          },
          (err) => {
            group.error = err;
            notify();
          },
        );
      } catch (err) {
        group.error = err;
        notify();
      }
    },
    destroy: () => {
      if (group.pollTimer) clearInterval(group.pollTimer);
      if (group.heartbeatTimer) clearInterval(group.heartbeatTimer);
      core.leave();
      channel?.close();
      groups.delete(key);
    },
  };

  core.onLeadershipChange = (isLeader) => {
    // A freshly promoted leader polls immediately instead of waiting an interval.
    if (isLeader) group.runQuery();
    notify();
  };
  core.onData = (payload) => {
    group.data = payload;
    group.loaded = true;
    group.error = undefined;
    notify();
  };
  core.onPoke = () => group.runQuery();

  if (channel) {
    channel.onmessage = (ev: MessageEvent) => {
      if (isLeaderMsg(ev.data)) core.receive(ev.data);
    };
  }

  core.announce();
  group.heartbeatTimer = setInterval(() => core.heartbeat(), HEARTBEAT_MS);
  group.pollTimer = setInterval(() => group.runQuery(), intervalMs);
  // Kick off an initial poll (leader-only inside runQuery).
  group.runQuery();

  groups.set(key, group);
  return group;
}

export interface SharedLeaderQueryResult<T> {
  data: T | undefined;
  isLeader: boolean;
  loading: boolean;
  error: unknown;
  /** Force a poll now (leader polls locally; a follower pokes the leader). */
  refetch: () => void;
}

/**
 * Subscribe a component to a key's shared, leader-elected poll.
 *
 * @param key        Stable identifier — all tabs/components using the same key
 *                   share one leader + one RPC stream.
 * @param queryFn    The RPC call. Only the leader tab ever invokes it.
 * @param intervalMs Poll cadence (default 5000ms).
 * @param enabled    When false, the hook detaches (no subscription/poll).
 */
export function useSharedLeaderQuery<T>(
  key: string,
  queryFn: () => Promise<T> | T,
  intervalMs = 5000,
  enabled = true,
): SharedLeaderQueryResult<T> {
  const [, forceRender] = useState(0);
  const queryRef = useRef(queryFn);
  queryRef.current = queryFn;

  useEffect(() => {
    if (!enabled) return;
    const group = ensureGroup(
      key,
      () => queryRef.current(),
      intervalMs,
    );
    group.refs += 1;
    const rerender = () => forceRender((n) => n + 1);
    group.subscribers.add(rerender);
    // Adopt the group's current snapshot immediately.
    rerender();

    return () => {
      group.subscribers.delete(rerender);
      group.refs -= 1;
      if (group.refs <= 0) group.destroy();
    };
  }, [key, intervalMs, enabled]);

  const group = enabled ? groups.get(key) : undefined;
  return {
    data: group?.data as T | undefined,
    isLeader: group?.core.isLeader() ?? true,
    loading: enabled ? !(group?.loaded ?? false) : false,
    error: group?.error,
    refetch: () => {
      const g = groups.get(key);
      if (!g) return;
      if (g.core.isLeader()) g.runQuery();
      else g.core.poke();
    },
  };
}
