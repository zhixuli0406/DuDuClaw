import type { DbSourceSummary } from '@/lib/api';

// ── AI 員工設定頁「可使用的資料庫來源」— pure helpers ─────────────────
//
// `capabilities.db_sources` is deny-by-default: an id absent from the list
// is invisible to the agent even though the source is configured elsewhere.
// This file builds the checkbox-list rows from the currently configured
// sources plus whatever the agent's own capability list already carries —
// including an id that no longer matches any configured source (the
// connection was renamed or removed after this agent was granted it), which
// must still render as a ticked, untickable-away row rather than silently
// vanish from the form (coding convention: never silently drop an operator's
// existing config).

export interface DbSourceCapabilityRow {
  id: string;
  label: string;
  checked: boolean;
  /** `true` when `id` has no matching entry in the currently configured
   *  source list — surfaced in the UI as "已不存在" so the operator can
   *  still see and untick it. */
  missing: boolean;
}

/** Build the checkbox-list rows: every configured source first (in the order
 *  `db_sources.list` returned them), then any selected id that isn't among
 *  them, appended and marked `missing`. */
export function buildDbSourceCapabilityRows(
  configured: readonly DbSourceSummary[],
  selected: readonly string[],
): DbSourceCapabilityRow[] {
  const selectedSet = new Set(selected);
  const configuredIds = new Set(configured.map((s) => s.name));
  const rows: DbSourceCapabilityRow[] = configured.map((s) => ({
    id: s.name,
    label: s.label || s.name,
    checked: selectedSet.has(s.name),
    missing: false,
  }));
  for (const id of selected) {
    if (!configuredIds.has(id)) {
      rows.push({ id, label: id, checked: true, missing: true });
    }
  }
  return rows;
}

/** Add `id` if absent, remove it if present. */
export function toggleDbSourceCapability(selected: readonly string[], id: string): string[] {
  return selected.includes(id) ? selected.filter((s) => s !== id) : [...selected, id];
}
