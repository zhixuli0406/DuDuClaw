import { describe, it, expect } from 'vitest';
import { buildDbSourceCapabilityRows, toggleDbSourceCapability } from './dbSourceCapability';
import type { DbSourceSummary } from '@/lib/api';

function dbSource(overrides: Partial<DbSourceSummary>): DbSourceSummary {
  return {
    name: 'crm_pg',
    label: 'CRM Postgres',
    driver: 'postgres',
    allowed_tables: ['customers'],
    max_rows: 200,
    timeout_ms: 10000,
    url_status: { configured: true, source: 'inline', source_label: 'encrypted', writable: true, residue: false },
    ...overrides,
  };
}

describe('buildDbSourceCapabilityRows', () => {
  it('lists configured sources unchecked when nothing is selected', () => {
    const rows = buildDbSourceCapabilityRows([dbSource({ name: 'crm_pg', label: '客戶 CRM' })], []);
    expect(rows).toEqual([{ id: 'crm_pg', label: '客戶 CRM', checked: false, missing: false }]);
  });

  it('checks configured sources that are selected', () => {
    const rows = buildDbSourceCapabilityRows(
      [dbSource({ name: 'crm_pg' }), dbSource({ name: 'ops_db', label: 'Ops DB' })],
      ['ops_db'],
    );
    expect(rows.find((r) => r.id === 'crm_pg')?.checked).toBe(false);
    expect(rows.find((r) => r.id === 'ops_db')?.checked).toBe(true);
  });

  it('falls back to the id as the label when a source has no label', () => {
    const rows = buildDbSourceCapabilityRows([dbSource({ name: 'crm_pg', label: '' })], []);
    expect(rows[0].label).toBe('crm_pg');
  });

  it('appends a selected id missing from the configured list, checked and marked missing', () => {
    const rows = buildDbSourceCapabilityRows([dbSource({ name: 'crm_pg' })], ['crm_pg', 'deleted_source']);
    expect(rows).toEqual([
      { id: 'crm_pg', label: 'CRM Postgres', checked: true, missing: false },
      { id: 'deleted_source', label: 'deleted_source', checked: true, missing: true },
    ]);
  });

  it('never drops a stale selected id even when nothing is configured', () => {
    const rows = buildDbSourceCapabilityRows([], ['gone']);
    expect(rows).toEqual([{ id: 'gone', label: 'gone', checked: true, missing: true }]);
  });

  it('returns an empty list when there is nothing configured and nothing selected', () => {
    expect(buildDbSourceCapabilityRows([], [])).toEqual([]);
  });
});

describe('toggleDbSourceCapability', () => {
  it('adds an absent id', () => {
    expect(toggleDbSourceCapability(['a'], 'b')).toEqual(['a', 'b']);
  });

  it('removes a present id (e.g. unticking a missing/stale row)', () => {
    expect(toggleDbSourceCapability(['a', 'b'], 'b')).toEqual(['a']);
  });

  it('never mutates the input array', () => {
    const input = ['a'];
    toggleDbSourceCapability(input, 'b');
    expect(input).toEqual(['a']);
  });
});
