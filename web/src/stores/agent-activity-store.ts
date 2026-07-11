import { create } from 'zustand';
import { client } from '@/lib/ws-client';
import type { ActivityEvent } from '@/lib/api';

/**
 * Live presence states for an AI-staff member's status glyph (WP10-T10.2).
 *
 * These are derived NON-INVASIVELY from the WS events the gateway already
 * broadcasts (`activity.new`, `browser.approval_request`) — we do not invent a
 * new backend truth source. A live state is transient: it decays back to
 * `idle` after a TTL so the glyph can never get stuck (there is no reliable
 * "activity ended" event to key off). The agent's persistent lifecycle status
 * (paused / terminated) always wins over any live state and is resolved by the
 * consumer, not stored here.
 */
export type AgentLiveState =
  | 'idle'
  | 'replying'
  | 'tool_running'
  | 'consolidating'
  | 'awaiting_approval';

interface LiveEntry {
  state: AgentLiveState;
  /** epoch ms after which this live state is stale and should read as idle. */
  expiresAt: number;
}

interface AgentActivityStore {
  /** agent_id → current live entry. Absent ⇒ idle. */
  readonly live: Readonly<Record<string, LiveEntry>>;
  /** Record a transient live state for an agent (used by the WS subscription). */
  bump: (agentId: string, state: AgentLiveState, ttlMs: number) => void;
  /** Drop stale entries. Called on a timer; a no-op returns the same object. */
  sweep: () => void;
}

// How long each derived state is considered "current" before decaying to idle.
const TTL: Record<AgentLiveState, number> = {
  idle: 0,
  replying: 18_000,
  tool_running: 12_000,
  consolidating: 40_000,
  awaiting_approval: 90_000,
};

// Priority when a fresher event of a different kind arrives — a higher number
// wins so a burst of activity resolves to the most salient state.
const PRIORITY: Record<AgentLiveState, number> = {
  idle: 0,
  tool_running: 1,
  replying: 2,
  consolidating: 3,
  awaiting_approval: 4,
};

/** Map an ActivityEvent type onto a live state (or null to ignore). */
function stateForActivity(type: ActivityEvent['type']): AgentLiveState | null {
  switch (type) {
    case 'agent_reply':
      return 'replying';
    case 'task_created':
    case 'task_assigned':
    case 'task_completed':
    case 'task_blocked':
    case 'autopilot_triggered':
      return 'tool_running';
    case 'skill_learned':
    case 'evolution_triggered':
      return 'consolidating';
    default:
      return null;
  }
}

export const useAgentActivityStore = create<AgentActivityStore>((set, get) => {
  const bump: AgentActivityStore['bump'] = (agentId, state, ttlMs) => {
    if (!agentId) return;
    const now = Date.now();
    const existing = get().live[agentId];
    // Keep the higher-priority state if the existing one is still fresh.
    if (
      existing &&
      existing.expiresAt > now &&
      PRIORITY[existing.state] > PRIORITY[state]
    ) {
      return;
    }
    set({ live: { ...get().live, [agentId]: { state, expiresAt: now + ttlMs } } });
  };

  // Subscribe once at module init — mirrors agents-store's pattern. The
  // gateway broadcasts these to every authenticated client.
  client.subscribe('activity.new', (payload) => {
    const ev = payload as ActivityEvent;
    const st = stateForActivity(ev?.type);
    if (st && ev?.agent_id) bump(ev.agent_id, st, TTL[st]);
  });

  client.subscribe('browser.approval_request', (payload) => {
    const p = payload as { agent_id?: string };
    if (p?.agent_id) bump(p.agent_id, 'awaiting_approval', TTL.awaiting_approval);
  });

  // Periodic decay so glyphs relax back to idle. Cheap no-op when idle.
  setInterval(() => get().sweep(), 3_000);

  return {
    live: {},
    bump,
    sweep: () => {
      const now = Date.now();
      const cur = get().live;
      const next: Record<string, LiveEntry> = {};
      let changed = false;
      for (const [id, entry] of Object.entries(cur)) {
        if (entry.expiresAt > now) next[id] = entry;
        else changed = true;
      }
      if (changed) set({ live: next });
    },
  };
});

/**
 * Resolve the effective glyph state for one agent: persistent lifecycle status
 * (paused/terminated) wins; otherwise the live derived state; otherwise idle.
 */
export type AgentGlyphState =
  | AgentLiveState
  | 'paused'
  | 'terminated';

export function useAgentGlyphState(
  agentId: string,
  status: string | undefined,
): AgentGlyphState {
  const entry = useAgentActivityStore((s) => s.live[agentId]);
  if (status === 'paused') return 'paused';
  if (status === 'terminated') return 'terminated';
  if (entry && entry.expiresAt > Date.now()) return entry.state;
  return 'idle';
}
