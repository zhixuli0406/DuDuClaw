import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, within } from '@testing-library/react';
import '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import type { AuditEvent } from '@/lib/api';
import { AuditTab, auditCategory } from './AuditTab';
import { calledWith, renderTab, routeCalls } from './test-helpers';

const event = (event_type: string, timestamp: string, details: Record<string, unknown> = {}, agent_id = 'isolation-connectors'): AuditEvent =>
  ({ event_type, timestamp, agent_id, severity: 'info', details });

const EVENTS: AuditEvent[] = [
  event('isolation.connector_provisioned', '2026-11-09T01:00:00Z', { actor_user_id: 'u-alice', provider: 'google' }),
  event('isolation.employee_tombstoned', '2026-11-09T03:00:00Z', { actor_user_id: 'u-bob', label: 'old-helper' }, 'old-helper'),
  event('isolation.employee_restored', '2026-11-09T04:00:00Z', { actor_user_id: 'u-bob', label: 'old-helper' }, 'old-helper'),
  event('isolation.personal_delegation_decided', '2026-11-09T05:00:00Z', {}),
  // Not a protected-mode record: never listed here.
  event('google_identity_settings_set', '2026-11-09T06:00:00Z', { actor_user_id: 'u-alice' }),
];

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
});

describe('protected mode — records tab', () => {
  it('classifies only protected-mode events', () => {
    expect(auditCategory('isolation.connector_revoked')).toBe('connector');
    expect(auditCategory('isolation.employee_tombstoned')).toBe('employee');
    expect(auditCategory('isolation.employee_restored')).toBe('restore');
    expect(auditCategory('isolation.employee_quarantine_purged')).toBe('restore');
    expect(auditCategory('isolation.personal_delegation_decided')).toBe('delegation');
    expect(auditCategory('isolation.recording_stop')).toBeNull();
    expect(auditCategory('agent_authority_changed')).toBeNull();
  });

  it('lists when, what and who, newest first, and filters by category', async () => {
    routeCalls({ 'security.audit_log': () => ({ events: EVENTS }) });
    renderTab(<AuditTab />);
    const table = await screen.findByRole('region', { name: 'Records' });
    const rows = within(table).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(4);
    expect(rows[0]).toHaveTextContent('Delegation approval');
    expect(rows[0]).toHaveTextContent('System');
    expect(rows[3]).toHaveTextContent('Connector account created · Google');
    expect(rows[3]).toHaveTextContent('Alice Chen');
    expect(table).not.toHaveTextContent('u-alice');
    expect(calledWith('security.audit_log')).toEqual([{ limit: 1000 }]);

    fireEvent.click(screen.getByRole('button', { name: 'Employee approvals and retirements' }));
    expect(screen.getByRole('button', { name: 'Employee approvals and retirements' })).toHaveAttribute('aria-pressed', 'true');
    const filtered = within(screen.getByRole('region', { name: 'Records' })).getAllByRole('row').slice(1);
    expect(filtered).toHaveLength(1);
    expect(filtered[0]).toHaveTextContent('AI employee retired · old-helper');
    expect(filtered[0]).toHaveTextContent('Bob Lin');

    fireEvent.click(screen.getByRole('button', { name: 'Restores and clean-ups' }));
    expect(within(screen.getByRole('region', { name: 'Records' })).getAllByRole('row').slice(1)[0])
      .toHaveTextContent('Restore or clean-up · old-helper');
  });

  it('shows the empty state for a category with no records', async () => {
    routeCalls({ 'security.audit_log': () => ({ events: [EVENTS[0]] }) });
    renderTab(<AuditTab />);
    await screen.findByRole('region', { name: 'Records' });
    fireEvent.click(screen.getByRole('button', { name: 'Delegation approvals' }));
    expect(screen.getByText('No matching records')).toBeInTheDocument();
  });

  it('shows the server’s refusal when records cannot load', async () => {
    routeCalls({ 'security.audit_log': () => Promise.reject('無法讀取稽核紀錄。') });
    renderTab(<AuditTab />);
    expect(await screen.findByRole('alert')).toHaveTextContent('無法讀取稽核紀錄。');
  });
});
