/**
 * Pure helpers for editing the org chart (RM-027): reading the structured
 * `{ code, message }` error the org RPCs reject with, spotting a reporting
 * cycle before the request is sent, and picking the drop target of a dragged
 * node. No React, no RPC — unit-testable in isolation.
 */
import type { OrgPendingChange, OrgPlacement } from '@/lib/api';

/** `error.code` from a rejected org RPC, or `null` for a plain/permission error. */
export function orgErrorCode(err: unknown): string | null {
  if (err && typeof err === 'object' && 'code' in err) {
    const code = (err as { code?: unknown }).code;
    if (typeof code === 'string' && code) return code;
  }
  return null;
}

/** Server-sent human message of a structured org error, if any. */
export function orgErrorMessage(err: unknown): string {
  if (err && typeof err === 'object' && 'message' in err) {
    const m = (err as { message?: unknown }).message;
    if (typeof m === 'string') return m;
  }
  return typeof err === 'string' ? err : '';
}

/** Extra field a structured org error carries (`current`, `current_base`, `base`, …). */
export function orgErrorField<T>(err: unknown, key: string): T | undefined {
  if (err && typeof err === 'object' && key in err) return (err as Record<string, T>)[key];
  return undefined;
}

/** Normalised placement (missing fields → empty string). */
export function normPlacement(p: Partial<OrgPlacement> | null | undefined): OrgPlacement {
  return { reports_to: p?.reports_to ?? '', department: p?.department ?? '' };
}

interface HasReportsTo {
  name: string;
  reports_to?: string;
}

/** Everyone below `name` in the reporting tree (excluding `name` itself). */
export function descendantsOf(agents: ReadonlyArray<HasReportsTo>, name: string): Set<string> {
  const out = new Set<string>();
  const queue = [name];
  while (queue.length > 0) {
    const current = queue.pop()!;
    for (const a of agents) {
      if (a.reports_to === current && a.name !== name && !out.has(a.name)) {
        out.add(a.name);
        queue.push(a.name);
      }
    }
  }
  return out;
}

/** True when making `managerId` the manager of `agentId` would loop the tree. */
export function wouldCycle(
  agents: ReadonlyArray<HasReportsTo>,
  agentId: string,
  managerId: string,
): boolean {
  if (!managerId) return false;
  if (managerId === agentId) return true;
  return descendantsOf(agents, agentId).has(managerId);
}

export interface NodeRect {
  name: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * The node a dragged card is released over, or `null`. The dragged node itself,
 * everything below it (it would form a cycle) and its current manager (no
 * change) are not valid targets.
 */
export function pickDropTarget(
  rects: ReadonlyArray<NodeRect>,
  point: { x: number; y: number },
  dragged: string,
  agents: ReadonlyArray<HasReportsTo>,
): string | null {
  const dragAgent = agents.find((a) => a.name === dragged);
  const blocked = descendantsOf(agents, dragged);
  blocked.add(dragged);
  if (dragAgent?.reports_to) blocked.add(dragAgent.reports_to);
  for (const r of rects) {
    if (blocked.has(r.name)) continue;
    if (point.x >= r.x && point.x <= r.x + r.w && point.y >= r.y && point.y <= r.y + r.h) {
      return r.name;
    }
  }
  return null;
}

export type OrgChangeRowStatus = 'pending' | 'approved' | 'needs_resubmit' | 'rejected';

/** Row of the 「待核准的調整」 table. */
export interface OrgChangeRow {
  change: OrgPendingChange;
  status: OrgChangeRowStatus;
}

/** A waiting request whose employee's placement moved since is only rejectable. */
export function pendingStatus(change: OrgPendingChange): OrgChangeRowStatus {
  return change.stale ? 'needs_resubmit' : 'pending';
}
