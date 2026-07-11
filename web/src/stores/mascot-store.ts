import { useMemo } from 'react';
import { create } from 'zustand';
import type { ConnectionState } from '@/lib/ws-client';
import type { DuduFace } from '@/components/mascot/faces';
import { computeMood, moodExpression } from '@/lib/mascot-mood';
import { useAgentsStore } from './agents-store';
import { useApprovalsStore } from './approvals-store';
import { useConnectionStore } from './connection-store';
import { useAgentActivityStore } from './agent-activity-store';
import { growthEventBus } from './growth-store';

/**
 * mascot-store — the DuDu companion's face is a *pure derivation* of live app
 * state (§7.2). There is no self-owned truth here and no polling: the face
 * falls out of the roster (agents-store), the "needs me" inbox
 * (approvals-store), live agent runs (agent-activity-store), and the socket
 * connection (connection-store). The old floating-mascot persistence
 * (position / minimised / mood emoji) is retired with the emoji mascot; the
 * Tauri desktop pet owns its own window visibility via the tray, and the
 * overlay page owns its transient hover state locally.
 */

export interface DuduFaceInput {
  /** Total AI staff known to the dashboard. */
  total: number;
  /** Count currently working (fresh live run). */
  live: number;
  /** Count of AI staff online (`active`). */
  active: number;
  /** Unified "needs me" inbox count. */
  inbox: number;
  /** Socket connection state. Anything below `authenticated` reads as offline. */
  connection: ConnectionState;
}

export interface DuduFaceResult {
  face: DuduFace;
  /** i18n message id for a one-line mood label. */
  labelId: string;
}

/**
 * Derive DuDu's face from a snapshot. Precedence:
 *  1. offline (socket not authenticated) → `concerned` (something's wrong).
 *  2. a non-empty inbox → `curious` (wants your attention).
 *  3. a live run in flight → `writing` (busy working).
 *  4. otherwise fall back to the four-way mood → face map.
 */
export function computeDuduFace(input: DuduFaceInput): DuduFaceResult {
  if (input.connection !== 'authenticated') {
    return { face: 'concerned', labelId: 'mascot.mood.alert' };
  }
  if (input.inbox > 0) {
    return moodExpression('poke'); // curious
  }
  if (input.live > 0) {
    return { face: 'writing', labelId: 'mascot.mood.focused' };
  }
  const mood = computeMood({ total: input.total, active: input.active, error: 0, inbox: 0 });
  return moodExpression(mood);
}

/**
 * Transient-face override (§7.2 / A2). The derived face is DuDu's steady state;
 * momentary emotional beats (a level-up, an achievement unlock) briefly *push* a
 * face on top of it, then expire back to the derivation. This is a self-owned
 * bit of truth — the one exception to the "pure derivation" rule — because the
 * triggering events are impulses, not states, so nothing to derive them from.
 */
interface TransientFaceStore {
  /** Active override, or null when DuDu should show its derived face. */
  face: DuduFace | null;
  /** Push `face` for `ms`, replacing any in-flight override. */
  setTransientFace: (face: DuduFace, ms: number) => void;
}

let transientTimer: ReturnType<typeof setTimeout> | null = null;

export const useMascotTransientStore = create<TransientFaceStore>((set) => ({
  face: null,
  setTransientFace: (face, ms) => {
    if (transientTimer) clearTimeout(transientTimer);
    set({ face });
    transientTimer = setTimeout(() => {
      transientTimer = null;
      set({ face: null });
    }, ms);
  },
}));

// Growth moments give DuDu a transient `proud` face for 4s (§6.5 / §7.2). The
// bus is a module singleton (same pattern as toast/celebrate), so a single
// module-level subscription is the right lifetime — it lives as long as the app.
// achievement_unlocked / level_up both read as "you did well" → proud.
growthEventBus.subscribe((ev) => {
  if (ev.type === 'achievement_unlocked' || ev.type === 'level_up') {
    useMascotTransientStore.getState().setTransientFace('proud', 4000);
  }
});

/**
 * Reactive hook: the single source of DuDu's face for both the in-app companion
 * and the Tauri desktop pet. A live transient override wins; otherwise the face
 * is a pure derivation that recomputes only when its inputs change.
 */
export function useDuduFace(): DuduFaceResult {
  const agents = useAgentsStore((s) => s.agents);
  const inbox = useApprovalsStore((s) => s.pendingCount);
  const connection = useConnectionStore((s) => s.state);
  const liveMap = useAgentActivityStore((s) => s.live);
  const transientFace = useMascotTransientStore((s) => s.face);

  const active = agents.filter((a) => a.status === 'active').length;
  // A "live" agent is one with a fresh (non-idle, unexpired) activity entry.
  const now = Date.now();
  const live = Object.values(liveMap).filter((e) => e.expiresAt > now).length;

  const derived = useMemo(
    () => computeDuduFace({ total: agents.length, active, live, inbox, connection }),
    [agents.length, active, live, inbox, connection],
  );

  if (transientFace) return { face: transientFace, labelId: 'mascot.mood.proud' };
  return derived;
}
