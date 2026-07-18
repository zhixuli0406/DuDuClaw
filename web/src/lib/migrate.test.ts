import { describe, it, expect } from 'vitest';
import type { MigrateItemStatus, MigratePlatform, MigrateVerdict } from './api';
import {
  MIGRATE_PLATFORMS,
  PAPERCLIP_EXPORT_CMD,
  migratePlatformCard,
  statusChipTone,
  verdictToneClass,
  statusLabelKey,
  verdictLabelKey,
  canScan,
  migrateScanArgs,
  migrateApplyArgs,
} from './migrate';

describe('MIGRATE_PLATFORMS', () => {
  it('lists the three source platforms in order', () => {
    expect(MIGRATE_PLATFORMS.map((p) => p.id)).toEqual(['openclaw', 'hermes', 'paperclip']);
  });

  it('gives openclaw / hermes a default source and no export requirement', () => {
    expect(migratePlatformCard('openclaw')).toMatchObject({
      defaultSource: '~/.openclaw',
      sourceRequired: false,
      needsExport: false,
    });
    expect(migratePlatformCard('hermes')).toMatchObject({
      defaultSource: '~/.hermes',
      sourceRequired: false,
      needsExport: false,
    });
  });

  it('requires paperclip to supply a source and a prior export', () => {
    expect(migratePlatformCard('paperclip')).toMatchObject({
      defaultSource: null,
      sourceRequired: true,
      needsExport: true,
    });
  });

  it('throws on an unknown platform', () => {
    expect(() => migratePlatformCard('nope' as MigratePlatform)).toThrow(/unknown migrate platform/);
  });

  it('embeds the full paperclip export command', () => {
    expect(PAPERCLIP_EXPORT_CMD).toContain('paperclipai company export');
    expect(PAPERCLIP_EXPORT_CMD).toContain('--include company,agents,projects,issues,tasks,skills');
  });
});

describe('statusChipTone', () => {
  const cases: Array<[MigrateItemStatus, string]> = [
    ['imported', 'success'],
    ['partial', 'warning'],
    ['skipped', 'neutral'],
    ['conflict', 'danger'],
  ];
  it.each(cases)('maps %s → %s', (status, tone) => {
    expect(statusChipTone(status)).toBe(tone);
  });
});

describe('verdictToneClass', () => {
  // Semantic tokens (MDS migration): success / warning / muted-foreground.
  const cases: Array<[MigrateVerdict, string]> = [
    ['COMPLETE', 'success'],
    ['DEGRADED', 'warning'],
    ['PARTIAL', 'muted-foreground'],
  ];
  it.each(cases)('%s uses a %s token class', (verdict, colour) => {
    expect(verdictToneClass(verdict)).toContain(colour);
  });
});

describe('i18n key helpers', () => {
  it('builds status label keys', () => {
    expect(statusLabelKey('conflict')).toBe('migrate.status.conflict');
  });
  it('builds verdict label keys', () => {
    expect(verdictLabelKey('DEGRADED')).toBe('migrate.verdict.DEGRADED');
  });
});

describe('canScan', () => {
  it('always allows platforms with a default source', () => {
    expect(canScan('openclaw', '')).toBe(true);
    expect(canScan('hermes', '   ')).toBe(true);
  });

  it('requires a non-empty source for paperclip', () => {
    expect(canScan('paperclip', '')).toBe(false);
    expect(canScan('paperclip', '   ')).toBe(false);
    expect(canScan('paperclip', './export')).toBe(true);
  });
});

describe('migrateScanArgs', () => {
  it('omits an empty / whitespace source (uses the gateway default)', () => {
    expect(migrateScanArgs('openclaw')).toEqual({ platform: 'openclaw' });
    expect(migrateScanArgs('openclaw', '')).toEqual({ platform: 'openclaw' });
    expect(migrateScanArgs('openclaw', '   ')).toEqual({ platform: 'openclaw' });
  });

  it('includes a trimmed source when provided', () => {
    expect(migrateScanArgs('paperclip', '  ./export  ')).toEqual({
      platform: 'paperclip',
      source: './export',
    });
  });
});

describe('migrateApplyArgs', () => {
  it('mirrors scan args and omits rename when falsy', () => {
    expect(migrateApplyArgs('hermes', '~/.hermes')).toEqual({
      platform: 'hermes',
      source: '~/.hermes',
    });
    expect(migrateApplyArgs('hermes', '~/.hermes', false)).toEqual({
      platform: 'hermes',
      source: '~/.hermes',
    });
  });

  it('adds rename:true only when enabled', () => {
    expect(migrateApplyArgs('openclaw', '', true)).toEqual({
      platform: 'openclaw',
      rename: true,
    });
  });
});
