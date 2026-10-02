import { useAuthStore } from '@/stores/auth-store';

/**
 * Non-admin `agent_id` contract (L2). The gateway's `check_agent_filter!`
 * (dispatch_org.rs) rejects an un-scoped `tasks.list` / `activity.list` /
 * `plans.list` from any non-admin with "agent_id parameter is required" — by
 * design. Callers that list "everything I can see" therefore fan out here:
 * one request per agent the viewer is bound to, merged back into the shape a
 * single call returns.
 *
 * Source of "accessible agents" is the auth store's `bindings` (from
 * `/api/me`, refreshed with every token) — the same set the server's
 * `agent_access` is built from, with no extra round-trip and naturally reset
 * on logout / login.
 */

/** Agents to fan out over, or `null` when the request must go out unchanged
 *  (an explicit `agent_id`, an admin, or a viewer whose role isn't loaded). */
function fanOutAgents(params: { agent_id?: string } | undefined): string[] | null {
  if (params?.agent_id) return null;
  const { user, bindings } = useAuthStore.getState();
  if (!user || user.role === 'admin') return null;
  return [...new Set(bindings.map((b) => b.agent_name).filter(Boolean))];
}

/** Max requests in flight per fan-out call (a manager bound to many agents
 *  must not open one RPC per agent all at once). */
export const FAN_OUT_CONCURRENCY = 6;

/** Optional, non-breaking marker on a fanned-out result: the first per-agent
 *  failure when SOME agents failed. Absent on full success and on the
 *  single-request path. Callers that surface errors should report it. */
export interface PartialFanOut {
  partial_error?: unknown;
}

/** Run `task` over `items` with at most `limit` in flight; results keep
 *  `items` order. */
async function settleWithLimit<T, R>(
  items: readonly T[],
  limit: number,
  task: (item: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results: PromiseSettledResult<R>[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      try {
        results[i] = { status: 'fulfilled', value: await task(items[i]) };
      } catch (reason) {
        results[i] = { status: 'rejected', reason };
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}

/**
 * Send `call(params)` once, or — for a non-admin listing without `agent_id` —
 * once per accessible agent (≤ `FAN_OUT_CONCURRENCY` in flight) and `merge`
 * the fulfilled results. Zero agents ⇒ `merge([])` with no request. Partial
 * failure keeps the agents that answered and sets `partial_error`; only a
 * total failure rejects (with the first error).
 */
export async function fanOutByAgent<P extends { agent_id?: string }, R>(
  params: P | undefined,
  call: (p: P | Record<string, never>) => Promise<R>,
  merge: (results: NonNullable<R>[]) => R,
): Promise<R & PartialFanOut> {
  const agents = fanOutAgents(params);
  if (agents === null) return call(params ?? {}) as Promise<R & PartialFanOut>;
  if (agents.length === 0) return merge([]) as R & PartialFanOut;
  const settled = await settleWithLimit(agents, FAN_OUT_CONCURRENCY, (agent_id) =>
    call({ ...(params ?? ({} as P)), agent_id }),
  );
  const ok = settled.flatMap((s) => (s.status === 'fulfilled' && s.value != null ? [s.value as NonNullable<R>] : []));
  const firstFailure = settled.find((s): s is PromiseRejectedResult => s.status === 'rejected');
  if (!firstFailure) return merge(ok) as R & PartialFanOut;
  if (ok.length === 0) throw firstFailure.reason;
  return { ...(merge(ok) as object), partial_error: firstFailure.reason } as R & PartialFanOut;
}

const ts = (v: string | undefined | null) => {
  const n = v ? Date.parse(v) : NaN;
  return Number.isNaN(n) ? 0 : n;
};

/** Concatenate, keep the first row per `id`, then stable-sort with `cmp`. */
export function mergeById<T extends { id: string }>(lists: T[][], cmp: (a: T, b: T) => number): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const list of lists) {
    for (const row of list ?? []) {
      if (seen.has(row.id)) continue;
      seen.add(row.id);
      out.push(row);
    }
  }
  return out.sort(cmp);
}

/** `tasks.list` order: `ORDER BY pinned DESC, updated_at DESC`. */
export const byPinnedThenUpdated = (
  a: { pinned?: boolean; updated_at: string },
  b: { pinned?: boolean; updated_at: string },
) => Number(!!b.pinned) - Number(!!a.pinned) || ts(b.updated_at) - ts(a.updated_at);

/** `tasks.flow_metrics` merge: per-agent `agents` rows concatenated and
 *  de-duplicated by `agent_id` (first seen wins, order kept); the top-level
 *  fields are already system-wide in every response (the gateway's
 *  `agent_id` filter narrows only `agents`), so they come from the first
 *  response and are never summed. No responses ⇒ `null` — the same "no
 *  metrics" value the task board already renders without a WIP gauge. */
export function mergeFlowMetrics<T extends { agents: { agent_id: string }[] }>(rs: T[]): T | null {
  if (rs.length === 0) return null;
  const seen = new Set<string>();
  const agents = rs.flatMap((r) => r.agents ?? []).filter((a) => {
    if (seen.has(a.agent_id)) return false;
    seen.add(a.agent_id);
    return true;
  });
  return { ...rs[0], agents };
}

/** `plans.list` order: `ORDER BY updated_at DESC`. */
export const byUpdatedDesc = (a: { updated_at: string }, b: { updated_at: string }) =>
  ts(b.updated_at) - ts(a.updated_at);

/** `activity.list` order: `ORDER BY timestamp DESC`. */
export const byTimestampDesc = (a: { timestamp: string }, b: { timestamp: string }) =>
  ts(b.timestamp) - ts(a.timestamp);
