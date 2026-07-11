import type { WorldObjectId } from './types';

/**
 * World object → route mapping (V8-T8.4). Every clickable prop has a Sidebar/⌘K
 * equivalent — the world is an *enhancement*, never the only path — so this is a
 * convenience layer. Manager-gated objects (door → channels, vault → billing)
 * return null when the viewer lacks the role, so a click is a no-op rather than
 * navigating to a page the RPC layer will reject anyway.
 */

export interface WorldNavContext {
  /** Viewer has manager+ role (from `hasMinRole(role, 'manager')`). */
  readonly isManager: boolean;
}

/**
 * Resolve the route for a world object. `agentId` is required for `'agent'`.
 * Returns null when the object has no action (coffee) or is gated off.
 */
export function worldObjectRoute(
  object: WorldObjectId,
  ctx: WorldNavContext,
  agentId?: string,
): string | null {
  switch (object) {
    case 'bulletin':
      return '/inbox';
    case 'whiteboard':
      return '/tasks';
    case 'door':
      return ctx.isManager ? '/manage/channels' : null;
    case 'vault':
      return ctx.isManager ? '/manage/billing' : null;
    case 'agent':
      return agentId ? `/agents/${encodeURIComponent(agentId)}` : null;
    case 'coffee':
      return null; // purely for fun — no navigation
    default:
      return null;
  }
}
