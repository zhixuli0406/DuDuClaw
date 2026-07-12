import { useEffect } from 'react';
import { create } from 'zustand';
import { api } from '@/lib/api';

/**
 * Agent avatar cache (WP4). `agents.list` only tells us *whether* an AI staff
 * member has an uploaded avatar (`has_avatar`) — the bytes live behind the
 * lightweight `agents.avatar` RPC (an inline data URI; E1 split it out of the
 * heavy `agents.inspect` so first-paint avatars don't drag a telemetry
 * aggregate + full config serialization along per staff member). This
 * module-level cache resolves those bytes lazily, once per agent per session, so
 * the uploaded image can appear consistently everywhere a `CharacterAvatar` is
 * drawn (roster, hero, sidebar, chat strip, …) without every call site
 * refetching.
 *
 * Honesty / cost: we only ever fetch for agents the roster has *seen* to have an
 * avatar (`known[id] === true`). Agents with no upload — the common case — cost
 * zero network. In-flight requests are deduped. A failed fetch caches `null`
 * (falls back to the generative character) and is not retried.
 */
interface AvatarStore {
  /** Resolved data URIs (null = no avatar / failed to load). */
  readonly cache: Readonly<Record<string, string | null>>;
  /** Whether each agent has an uploaded avatar, seeded from `agents.list`. */
  readonly known: Readonly<Record<string, boolean>>;
  readonly inflight: ReadonlySet<string>;
  /** Seed `has_avatar` flags from a roster listing. */
  seed: (agents: ReadonlyArray<{ name: string; has_avatar?: boolean }>) => void;
  /** Lazily resolve one agent's avatar bytes (no-op if unknown / already done). */
  resolve: (agentId: string) => void;
  /** Directly set (or clear) an agent's cached avatar after an upload/remove. */
  set: (agentId: string, dataUri: string | null) => void;
}

export const useAgentAvatarStore = create<AvatarStore>((set, get) => ({
  cache: {},
  known: {},
  inflight: new Set<string>(),
  seed: (agents) => {
    const known: Record<string, boolean> = { ...get().known };
    let changed = false;
    for (const a of agents) {
      const has = a.has_avatar === true;
      if (known[a.name] !== has) {
        known[a.name] = has;
        changed = true;
      }
    }
    if (changed) set({ known });
  },
  resolve: (agentId) => {
    const { cache, known, inflight } = get();
    if (agentId in cache || inflight.has(agentId)) return;
    if (known[agentId] !== true) return; // only fetch when we know there's one
    const next = new Set(inflight);
    next.add(agentId);
    set({ inflight: next });
    api.agents
      .avatar(agentId)
      .then((d) => get().set(agentId, d.avatar ?? null))
      .catch(() => get().set(agentId, null))
      .finally(() => {
        const cur = new Set(get().inflight);
        cur.delete(agentId);
        set({ inflight: cur });
      });
  },
  set: (agentId, dataUri) =>
    set((s) => ({
      cache: { ...s.cache, [agentId]: dataUri },
      known: { ...s.known, [agentId]: dataUri != null },
    })),
}));

/**
 * Resolve one agent's uploaded avatar data URI (or `undefined` while unknown /
 * unresolved). Safe to call for non-agent ids (author names, etc.) — it simply
 * never resolves and the caller falls back to the generative character.
 */
export function useAgentAvatar(agentId: string | undefined): string | undefined {
  const cached = useAgentAvatarStore((s) => (agentId ? s.cache[agentId] : undefined));
  const resolve = useAgentAvatarStore((s) => s.resolve);
  useEffect(() => {
    if (agentId) resolve(agentId);
  }, [agentId, resolve]);
  return cached ?? undefined;
}
