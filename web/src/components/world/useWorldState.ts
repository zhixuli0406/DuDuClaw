import { useEffect, useMemo, useRef, useState } from 'react';
import { effectiveTint, type AgentOutfit } from '@/lib/outfit';
import { useAgentActivityStore, type AgentLiveState } from '@/stores/agent-activity-store';
import type { SceneDefinition, WorldAgentState, WorldEmote } from './types';
import { truncateGraphemes } from './text';

/**
 * useWorldState (V8-T8.3) — the state-driven controller. It subscribes to the
 * agents list (passed in, already data-scoped) and the derived live-activity
 * store, and outputs an authoritative {@link WorldAgentState} per agent. The
 * renderer only *interpolates* toward these; all behaviour logic lives here.
 *
 * Behaviour map (§8.2), now delegated to the active {@link SceneDefinition} so
 * office / town / lounge each place agents their own way (desks vs building
 * doors vs sofas). The emote + nameplate + bubble logic stays scene-agnostic.
 * A scene may hide an agent entirely (lounge hides busy workers) by returning
 * `null` from `place` — those agents are dropped from the output.
 *
 * Speech bubbles: when an agent's live signal changes to a non-idle state, a
 * head-top bubble appears for {@link BUBBLE_TTL_MS} then fades. Text is the
 * caller-supplied (i18n-resolved) phrase for that state — CJK-safe truncated.
 */

/** Minimal agent shape the stage needs (matches `WorldStageAgent`). */
export interface WorldInputAgent {
  readonly name: string;
  readonly display_name: string;
  readonly status: 'active' | 'paused' | 'terminated';
  /** Wardrobe composition; null/absent = seeded default look. */
  readonly outfit?: AgentOutfit | null;
}

/** How long a speech bubble stays before the renderer fades it. */
export const BUBBLE_TTL_MS = 8_000;

/** Max graphemes shown in a speech bubble. */
const BUBBLE_MAX = 18;

/** Map a live activity state to a head-top emote (or null when calm). */
export function emoteForLive(status: WorldInputAgent['status'], live: AgentLiveState): WorldEmote | null {
  if (status === 'terminated') return null;
  if (status === 'paused') return 'sleeping';
  if (live === 'awaiting_approval') return 'awaiting';
  if (live === 'idle') return null;
  return 'working';
}

/**
 * Pure core: map one agent + its resolved live state to a WorldAgentState using
 * the active scene's placement rule. `index` fixes the desk / seat / door; `say`
 * is an already-truncated bubble string or undefined. Returns `null` when the
 * scene hides this agent (e.g. a busy worker in the lounge).
 */
export function mapAgentToWorldState(
  scene: SceneDefinition,
  agent: WorldInputAgent,
  index: number,
  live: AgentLiveState,
  say?: string,
): WorldAgentState | null {
  const placement = scene.place({ index, status: agent.status, busy: live !== 'idle' });
  if (!placement) return null;
  const tintIndex = effectiveTint(agent.name, agent.outfit);
  return {
    id: agent.name,
    label: agent.display_name,
    tintIndex,
    outfit: agent.outfit ?? null,
    say,
    x: placement.x,
    y: placement.y,
    action: placement.action,
    facing: placement.facing,
    emote: emoteForLive(agent.status, live),
    dimmed: placement.dimmed,
  };
}

interface Bubble {
  text: string;
  expiresAt: number;
}

/**
 * React hook binding the stores to WorldAgentState output. `phraseFor` resolves
 * an i18n bubble string for a live state (kept out of the pure mapper so the
 * mapping stays locale-agnostic and testable).
 */
export function useWorldState(
  agents: ReadonlyArray<WorldInputAgent>,
  opts: { phraseFor: (state: AgentLiveState) => string; scene: SceneDefinition },
): ReadonlyArray<WorldAgentState> {
  const scene = opts.scene;
  const liveMap = useAgentActivityStore((s) => s.live);
  const bubbles = useRef<Map<string, Bubble>>(new Map());
  const lastState = useRef<Map<string, AgentLiveState>>(new Map());
  const [, force] = useState(0);
  const phraseFor = opts.phraseFor;

  // Resolve the effective live state per agent (expired ⇒ idle).
  const resolvedLive = useMemo(() => {
    const now = Date.now();
    const out = new Map<string, AgentLiveState>();
    for (const a of agents) {
      const e = liveMap[a.name];
      out.set(a.name, e && e.expiresAt > now ? e.state : 'idle');
    }
    return out;
  }, [agents, liveMap]);

  // Spawn a bubble when a live state transitions into a non-idle value.
  useEffect(() => {
    const now = Date.now();
    for (const a of agents) {
      const cur = resolvedLive.get(a.name) ?? 'idle';
      const prev = lastState.current.get(a.name) ?? 'idle';
      if (cur !== prev && cur !== 'idle' && a.status === 'active') {
        bubbles.current.set(a.name, {
          text: truncateGraphemes(phraseFor(cur), BUBBLE_MAX),
          expiresAt: now + BUBBLE_TTL_MS,
        });
      }
      lastState.current.set(a.name, cur);
    }
  }, [agents, resolvedLive, phraseFor]);

  // Sweep expired bubbles so they fade out.
  useEffect(() => {
    const t = setInterval(() => {
      const now = Date.now();
      let changed = false;
      for (const [id, b] of bubbles.current) {
        if (b.expiresAt <= now) {
          bubbles.current.delete(id);
          changed = true;
        }
      }
      if (changed) force((n) => n + 1);
    }, 1_000);
    return () => clearInterval(t);
  }, []);

  return useMemo(() => {
    const now = Date.now();
    const out: WorldAgentState[] = [];
    agents.forEach((a, i) => {
      const live = resolvedLive.get(a.name) ?? 'idle';
      const b = bubbles.current.get(a.name);
      const say = b && b.expiresAt > now ? b.text : undefined;
      const state = mapAgentToWorldState(scene, a, i, live, say);
      if (state) out.push(state);
    });
    return out;
    // bubbles is a ref; the `force` counter re-runs this via re-render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents, resolvedLive, scene]);
}
