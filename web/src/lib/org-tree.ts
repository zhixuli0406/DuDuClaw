/**
 * Human reporting lines (`users.manager_user_id`), built client-side for the
 * Users page org chart and the direct-manager picker.
 *
 * The gateway is the authority: it refuses a cycle, a chain deeper than
 * {@link MAX_ORG_DEPTH} and a manager whose role cannot manage. These helpers
 * only shape what the dashboard shows, and they never trust the data to be
 * a clean tree: every walk carries a visited set and a depth cap, so a
 * cycle written straight into the database cannot hang the page, and a
 * person no walk reaches still appears (under 「未指定主管」).
 */

/** Same cap as the gateway (`duduclaw_auth::org::MAX_ORG_DEPTH`). */
export const MAX_ORG_DEPTH = 8;

/** The fields these helpers need from a user row. */
export interface OrgPerson {
  id: string;
  role: string;
  status: string;
  manager_user_id?: string | null;
}

export interface OrgNode<T extends OrgPerson> {
  person: T;
  reports: OrgNode<T>[];
}

export interface OrgTree<T extends OrgPerson> {
  /** People at the top of a reporting line who have at least one report. */
  roots: OrgNode<T>[];
  /** Everyone else: no manager and no reports, or not reachable from a top
   *  (a manager that is gone, or a cycle written by hand). */
  unassigned: T[];
}

const MANAGER_ROLES = new Set(['manager', 'admin']);

/** Whether this role may be someone's direct manager (Q2: never Employee). */
export function canManage(role: string): boolean {
  return MANAGER_ROLES.has(role);
}

function managerOf<T extends OrgPerson>(p: T, byId: Map<string, T>): string | null {
  const m = p.manager_user_id?.trim();
  return m && m !== p.id && byId.has(m) ? m : null;
}

/** Direct reports by manager id, in input order. */
function reportsIndex<T extends OrgPerson>(people: T[], byId: Map<string, T>): Map<string, T[]> {
  const index = new Map<string, T[]>();
  for (const p of people) {
    const m = managerOf(p, byId);
    if (!m) continue;
    const list = index.get(m) ?? [];
    list.push(p);
    index.set(m, list);
  }
  return index;
}

/**
 * Build the org chart. Offboarded people are left out (they are not part
 * of anyone's team); suspended people stay, as the gateway keeps them.
 */
export function buildOrgTree<T extends OrgPerson>(users: T[]): OrgTree<T> {
  const people = users.filter((u) => u.status !== 'offboarded');
  const byId = new Map(people.map((p) => [p.id, p]));
  const index = reportsIndex(people, byId);
  const placed = new Set<string>();

  const grow = (person: T, depth: number): OrgNode<T> => {
    placed.add(person.id);
    const reports =
      depth >= MAX_ORG_DEPTH
        ? []
        : (index.get(person.id) ?? [])
            .filter((r) => !placed.has(r.id))
            .map((r) => grow(r, depth + 1));
    return { person, reports };
  };

  const roots: OrgNode<T>[] = [];
  for (const p of people) {
    if (managerOf(p, byId) === null && (index.get(p.id)?.length ?? 0) > 0) {
      roots.push(grow(p, 1));
    }
  }
  const unassigned = people.filter((p) => !placed.has(p.id));
  return { roots, unassigned };
}

/** `userId` and everyone below them (bounded, cycle-safe). */
export function subtreeIds<T extends OrgPerson>(users: T[], userId: string): Set<string> {
  const byId = new Map(users.map((u) => [u.id, u]));
  const index = reportsIndex(users, byId);
  const seen = new Set<string>([userId]);
  let frontier = [userId];
  for (let depth = 0; depth < MAX_ORG_DEPTH && frontier.length > 0; depth += 1) {
    const next: string[] = [];
    for (const id of frontier) {
      for (const r of index.get(id) ?? []) {
        if (!seen.has(r.id)) {
          seen.add(r.id);
          next.push(r.id);
        }
      }
    }
    frontier = next;
  }
  return seen;
}

/**
 * Who may be offered as `userId`'s direct manager: active, role manager or
 * admin, and not `userId` or anyone below them (that would be a cycle).
 * `userId` null = a person being created (no subtree yet).
 */
export function managerCandidates<T extends OrgPerson>(users: T[], userId: string | null): T[] {
  const excluded = userId ? subtreeIds(users, userId) : new Set<string>();
  return users.filter((u) => u.status === 'active' && canManage(u.role) && !excluded.has(u.id));
}

/** Stable reporting-line error codes the gateway returns (`users.create` /
 *  `users.update`), mapped to an i18n message id. */
const ORG_ERROR_MESSAGES: Record<string, string> = {
  manager_cycle: 'users.manager.error.cycle',
  manager_too_deep: 'users.manager.error.tooDeep',
  manager_role: 'users.manager.error.role',
  manager_has_reports: 'users.manager.error.hasReports',
  manager_not_found: 'users.manager.error.notFound',
  manager_inactive: 'users.manager.error.inactive',
};

/** The i18n id for a coded reporting-line refusal, or null for any other
 *  error (the caller then falls back to its generic message). */
export function orgErrorMessageId(err: unknown): string | null {
  if (!err || typeof err !== 'object' || !('code' in err)) return null;
  const code = (err as { code?: unknown }).code;
  return typeof code === 'string' ? (ORG_ERROR_MESSAGES[code] ?? null) : null;
}
