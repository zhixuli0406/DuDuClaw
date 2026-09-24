import type { DbSourceGrantAgent, DbSourceGrantSource } from '@/lib/api';

// ── "誰能使用這個資料庫來源" — pure helpers shared by three surfaces
// (redaction 精靈 step 4 for db sources, RedactionSystemsCard's grant
// dialog, and — indirectly — the same checkbox-list shape on the AI 員工
// 設定頁) ───────────────────────────────────────────────────────────────
//
// Kept separate from `redactionSystems.ts` because that file is scoped to
// the merged "外部系統與資料來源" card's own row/egress logic; this one is
// scoped to the grants relation itself (`db_sources.grants.*`) and the one
// shared picker component built on top of it (`DbSourceAgentPicker.tsx`).

/** One row in the agent picker. */
export interface DbSourceAgentPickerRow {
  id: string;
  displayName: string;
  archived: boolean;
  checked: boolean;
}

/** Active agents first (in the order the RPC returned them), archived last —
 *  archived agents stay selectable (an operator may be re-onboarding one)
 *  but are visually pushed to the bottom and muted by the component. */
export function orderAgentsForPicker<T extends { archived: boolean }>(
  agents: readonly T[],
): T[] {
  const active = agents.filter((a) => !a.archived);
  const archived = agents.filter((a) => a.archived);
  return [...active, ...archived];
}

/** Build the picker's rows from the full agent roster plus the currently
 *  selected id set. Pure — the component owns no derived state of its own. */
export function buildAgentPickerRows(
  agents: readonly DbSourceGrantAgent[],
  selected: readonly string[],
): DbSourceAgentPickerRow[] {
  const selectedSet = new Set(selected);
  return orderAgentsForPicker(agents).map((a) => ({
    id: a.id,
    displayName: a.display_name || a.id,
    archived: a.archived,
    checked: selectedSet.has(a.id),
  }));
}

/** Add `id` if absent, remove it if present — the one toggle every picker
 *  instance shares instead of each owning its own splice logic. */
export function toggleAgentSelection(selected: readonly string[], id: string): string[] {
  return selected.includes(id) ? selected.filter((s) => s !== id) : [...selected, id];
}

/** How many AI employees a configured source is currently granted to — the
 *  systems card's "N 位 AI 員工可用" badge count. `0` both when the source
 *  genuinely has no grants and when the source has no entry yet in
 *  `sources` (e.g. `grants.list` hasn't caught up with a source created a
 *  moment ago) — the badge can't tell those apart and doesn't need to. */
export function countGrantsForSource(sources: readonly DbSourceGrantSource[], name: string): number {
  return sources.find((s) => s.name === name)?.agents.length ?? 0;
}

/** The agent id list already granted a source — used to prefill the picker
 *  both when editing an existing source in the wizard and when opening the
 *  systems card's grant dialog. */
export function grantedAgentsForSource(sources: readonly DbSourceGrantSource[], name: string): string[] {
  return sources.find((s) => s.name === name)?.agents ?? [];
}

/** One agent's stale db-source references, for the systems card's compact
 *  debt notice. */
export interface StaleDbSourceEntry {
  agentId: string;
  ids: string[];
}

/** Flatten the `stale` map into a stable, renderable list — filters out any
 *  entry whose id list happens to be empty (defensive; the backend should
 *  never send one, but an empty array must never render as a phantom row). */
export function buildStaleDbSourceEntries(stale: Record<string, string[]>): StaleDbSourceEntry[] {
  return Object.entries(stale)
    .filter(([, ids]) => ids.length > 0)
    .map(([agentId, ids]) => ({ agentId, ids }));
}

/** Resolve a stale entry's agent id to its display name using the same
 *  `grants.list` payload — falls back to the raw id when the agent was
 *  removed entirely (not merely archived) between the two calls. */
export function resolveAgentDisplayName(agents: readonly DbSourceGrantAgent[], id: string): string {
  return agents.find((a) => a.id === id)?.display_name || id;
}
