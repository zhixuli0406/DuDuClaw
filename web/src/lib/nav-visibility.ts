import type { UserRole } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';

/**
 * Shared visibility predicate for nav items and launcher cards
 * (TODO-genspark-workspace-shell §P3.1). Keeps the sidebar and the workspace
 * launcher grid using one rule so role/edition gating never drifts between them.
 *
 * Mirrors `Sidebar`'s logic: role-gated by `minRole`, and enterprise-only
 * surfaces are hidden on the `personal` edition. An absent `minRole` is visible
 * to everyone; an absent edition profile is treated as enterprise (show all).
 */
export interface Gated {
  readonly minRole?: UserRole;
  readonly enterprise?: boolean;
}

export function isVisible(
  item: Gated,
  userRole: UserRole | undefined,
  isPersonal: boolean,
): boolean {
  return hasMinRole(userRole, item.minRole) && !(isPersonal && item.enterprise);
}

/** Filter a list of gated items down to those the current user may see. */
export function filterVisible<T extends Gated>(
  items: readonly T[],
  userRole: UserRole | undefined,
  isPersonal: boolean,
): T[] {
  return items.filter((item) => isVisible(item, userRole, isPersonal));
}
