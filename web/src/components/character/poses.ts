import type { StatusEmoteKind } from './StatusEmote';

/**
 * Character poses (§3.2). A pose is a whole-body attitude — the bust variant
 * varies arms, eyes, and mouth by pose; the small avatar variant only reflects
 * the eyes/mouth part. Kept separate from the SVG so the semantic mapping from
 * real agent state → pose is pure and testable.
 */
export type CharacterPose =
  | 'idle'
  | 'working'
  | 'blocked'
  | 'sleeping'
  | 'celebrating'
  | 'waving';

/** Backend agent lifecycle (mirrors `AgentInfo['status']`). */
export type AgentLifecycle = 'active' | 'paused' | 'terminated';

/**
 * Map an agent's real state to a pose. `hasLiveRun` distinguishes an active
 * agent that's actually mid-task (working) from one that's merely online (idle).
 */
export function agentPose(status: AgentLifecycle, hasLiveRun = false): CharacterPose {
  switch (status) {
    case 'active':
      return hasLiveRun ? 'working' : 'idle';
    case 'paused':
    case 'terminated':
      return 'sleeping';
    default:
      return 'idle';
  }
}

/**
 * The head-top emote for an agent's state, or `null` when nothing needs saying
 * (a calm, idle-online agent wears no bubble). Kept aligned with `agentPose`.
 */
export function agentEmote(
  status: AgentLifecycle,
  hasLiveRun = false,
): StatusEmoteKind | null {
  if (status === 'paused' || status === 'terminated') return 'sleeping';
  if (status === 'active' && hasLiveRun) return 'working';
  return null;
}
