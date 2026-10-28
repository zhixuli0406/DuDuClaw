/**
 * The single "交辦任務" entry point.
 *
 * Every "give an AI employee something to do" button (sidebar, mobile ＋,
 * task board, `/goals`, agent cards, the gallery) navigates to the `/assign`
 * page. A seed (employee, goal text, criteria, mode) travels in router state,
 * never in the URL — a goal paragraph does not belong in a query string, a
 * bookmark or a server log. A seeded entry opens the 「複雜任務」 tab; a plain
 * one opens 「一般」. `?tab=complex` also selects the second tab.
 */

export const ASSIGN_PATH = '/assign';

/** Which of the three execution paths the 複雜任務 form submits to. */
export type AssignMode =
  /** 問一問 — a plain conversation on `/chat`; nothing is filed, nothing loops. */
  | 'ask'
  /** 交辦 — `tasks.goal_create`, i.e. the autonomous goal loop. */
  | 'assign'
  /** 想一想 (I-1c) — `tasks.goal_create` with `plan_first: true`: a plan is
   *  generated and parked for approval before any execution starts. */
  | 'plan';

export interface AssignPrefill {
  /** Preselect an AI employee (agent `name`). */
  readonly agentId?: string;
  /** Seed the goal text (e.g. "做一個同款" / a team task example). */
  readonly description?: string;
  /** Seed the acceptance criteria. */
  readonly acceptanceCriteria?: string;
  /** Open straight into a given mode; defaults to `assign`. */
  readonly mode?: AssignMode;
}

export type AssignTab = 'general' | 'complex';

const MODES: readonly AssignMode[] = ['ask', 'assign', 'plan'];

/** Router state for `/assign`. */
export function assignState(prefill?: AssignPrefill): { assignPrefill: AssignPrefill } | undefined {
  return prefill ? { assignPrefill: prefill } : undefined;
}

/** Read a seed back out of router state, keeping only well-typed fields. */
export function readAssignPrefill(state: unknown): AssignPrefill | null {
  const raw = (state as { assignPrefill?: unknown } | null)?.assignPrefill;
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
  const mode = typeof r.mode === 'string' && (MODES as readonly string[]).includes(r.mode)
    ? (r.mode as AssignMode)
    : undefined;
  return {
    agentId: str(r.agentId),
    description: str(r.description),
    acceptanceCriteria: str(r.acceptanceCriteria),
    mode,
  };
}

/** The tab to open: a seed or `?tab=complex` ⇒ 複雜任務, otherwise 一般. */
export function initialAssignTab(prefill: AssignPrefill | null, tabParam: string | null): AssignTab {
  if (prefill) return 'complex';
  return tabParam === 'complex' ? 'complex' : 'general';
}
