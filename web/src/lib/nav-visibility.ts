import type { UserRole } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';

/**
 * Shared visibility predicate for nav items and launcher cards
 * (TODO-genspark-workspace-shell §P3.1, extended by dashboard-redesign WP11-T11.1).
 * Keeps the sidebar and the workspace launcher grid using one rule so
 * role/edition gating never drifts between them.
 *
 * Mirrors `Sidebar`'s logic: role-gated by `minRole`, and enterprise-only
 * surfaces are hidden on the `personal` edition. An absent `minRole` is visible
 * to everyone; an absent edition profile is treated as enterprise (show all).
 *
 * Two finer-grained flags were added for the boss-oriented IA (§3.4 matrix):
 *  - `ownScope`  — a DATA-scope hint, NOT a visibility gate. Marks a page whose
 *    content an `employee` sees filtered to only their own bound agents (Home /
 *    Tasks / Memory / Agents). It never hides the nav entry; the data layer
 *    (`useDataScope`) consumes it. `isVisible` deliberately ignores it.
 *  - `operatorOnly` — a sensitive surface (per-user cost detail, redaction
 *    editing, security-posture widget, private-matter flags). Hidden unless the
 *    viewer has at least one `operator`/`owner` agent binding. Fail-closed: when
 *    the caller cannot supply that fact, the surface stays hidden.
 *
 * NOTE: front-end hiding is UX only. The gateway RPC layer is the real gate
 * (WP11-T11.6, fail-closed). Never rely on `isVisible` for security.
 */
export interface Gated {
  readonly minRole?: UserRole;
  readonly enterprise?: boolean;
  /** Data-scope hint (employee sees only own agents). Does not gate visibility. */
  readonly ownScope?: boolean;
  /** Sensitive surface — requires operator/owner access to be shown. */
  readonly operatorOnly?: boolean;
  /**
   * Progressive disclosure: hide until the named data actually exists, so
   * never-used features don't occupy nav space with dead pages. `'forks'` =
   * at least one RFC-26 fork record exists (see `useForksExist`). The route
   * itself stays reachable by URL — this is presentation, not access control.
   */
  readonly requiresData?: 'forks';
}

/** Extra context needed to evaluate the finer-grained gates. All optional so
 *  existing call sites keep compiling; omitted facts fail closed. */
export interface VisibilityContext {
  /** True when the viewer holds at least one operator/owner agent binding. */
  readonly hasOperatorAccess?: boolean;
  /** True once at least one fork record exists (progressive disclosure). */
  readonly forksExist?: boolean;
}

export function isVisible(
  item: Gated,
  userRole: UserRole | undefined,
  isPersonal: boolean,
  ctx?: VisibilityContext,
): boolean {
  if (!hasMinRole(userRole, item.minRole)) return false;
  if (isPersonal && item.enterprise) return false;
  // Sensitive surfaces stay hidden unless operator access is proven (fail-closed).
  if (item.operatorOnly && !ctx?.hasOperatorAccess) return false;
  // Progressive disclosure: hidden until the backing data exists.
  if (item.requiresData === 'forks' && !ctx?.forksExist) return false;
  return true;
}

/** Filter a list of gated items down to those the current user may see. */
export function filterVisible<T extends Gated>(
  items: readonly T[],
  userRole: UserRole | undefined,
  isPersonal: boolean,
  ctx?: VisibilityContext,
): T[] {
  return items.filter((item) => isVisible(item, userRole, isPersonal, ctx));
}
