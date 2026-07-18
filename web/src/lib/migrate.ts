import type { MigrateItemStatus, MigratePlatform, MigrateVerdict } from './api';

/**
 * Pure, framework-free helpers for the migration wizard (`/manage/migrate`).
 * Kept out of the React component so the platform metadata, status→style
 * mapping, and RPC argument assembly are unit-testable in isolation.
 */

export interface MigratePlatformCard {
  id: MigratePlatform;
  /** Placeholder / default source dir for the input. `null` ⇒ no default. */
  defaultSource: string | null;
  /** paperclip must supply an export directory before scanning. */
  sourceRequired: boolean;
  /** paperclip needs a prior CLI export before it can be imported. */
  needsExport: boolean;
}

/** The three source platforms, in display order. */
export const MIGRATE_PLATFORMS: readonly MigratePlatformCard[] = [
  { id: 'openclaw', defaultSource: '~/.openclaw', sourceRequired: false, needsExport: false },
  { id: 'hermes', defaultSource: '~/.hermes', sourceRequired: false, needsExport: false },
  { id: 'paperclip', defaultSource: null, sourceRequired: true, needsExport: true },
];

/** The exact command a paperclip operator runs to produce an importable export. */
export const PAPERCLIP_EXPORT_CMD =
  'paperclipai company export <company-id> --out ./export --include company,agents,projects,issues,tasks,skills';

/** Look up a platform card by id. Throws on an unknown platform (fail-loud). */
export function migratePlatformCard(id: MigratePlatform): MigratePlatformCard {
  const found = MIGRATE_PLATFORMS.find((p) => p.id === id);
  if (!found) throw new Error(`unknown migrate platform: ${id}`);
  return found;
}

/**
 * Badge tone for an item status. Maps to the shared `Badge` component's tones so
 * colours stay token-only: imported→emerald, partial→amber, skipped→stone,
 * conflict→rose.
 */
export type MigrateBadgeTone = 'success' | 'warning' | 'neutral' | 'danger';

export function statusChipTone(status: MigrateItemStatus): MigrateBadgeTone {
  switch (status) {
    case 'imported':
      return 'success';
    case 'partial':
      return 'warning';
    case 'skipped':
      return 'neutral';
    case 'conflict':
      return 'danger';
  }
}

/** Headline colour for the overall verdict (token-only utility classes). */
export function verdictToneClass(verdict: MigrateVerdict): string {
  switch (verdict) {
    case 'COMPLETE':
      return 'text-success';
    case 'DEGRADED':
      return 'text-warning';
    case 'PARTIAL':
      return 'text-muted-foreground';
  }
}

/** i18n message id for an item status label. */
export function statusLabelKey(status: MigrateItemStatus): string {
  return `migrate.status.${status}`;
}

/** i18n message id for a verdict label. */
export function verdictLabelKey(verdict: MigrateVerdict): string {
  return `migrate.verdict.${verdict}`;
}

/**
 * Whether the scan action may proceed given the current source input. Platforms
 * that require a source (paperclip) need a non-empty value; others are always OK.
 */
export function canScan(platform: MigratePlatform, source: string): boolean {
  const card = migratePlatformCard(platform);
  if (card.sourceRequired) return source.trim().length > 0;
  return true;
}

/**
 * Assemble the `migrate.scan` RPC params. An empty/whitespace source is omitted
 * so the gateway falls back to the platform default.
 */
export function migrateScanArgs(
  platform: MigratePlatform,
  source?: string,
): Record<string, unknown> {
  const trimmed = source?.trim();
  return { platform, ...(trimmed ? { source: trimmed } : {}) };
}

/**
 * Assemble the `migrate.apply` RPC params. Includes `rename:true` only when
 * auto-rename is enabled; otherwise the key is omitted (server default = skip).
 */
export function migrateApplyArgs(
  platform: MigratePlatform,
  source?: string,
  rename?: boolean,
): Record<string, unknown> {
  return { ...migrateScanArgs(platform, source), ...(rename ? { rename: true } : {}) };
}
