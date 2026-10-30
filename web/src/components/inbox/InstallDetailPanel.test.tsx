import { describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { InstallRequestInfo } from '@/lib/api';
import en from '@/i18n/en.json';
import { InstallDetailPanel } from './InstallDetailPanel';

vi.mock('@/lib/ws-client', () => ({ client: { call: vi.fn(), subscribe: vi.fn() } }));

function request(overrides: Partial<InstallRequestInfo> = {}): InstallRequestInfo {
  return {
    id: 'install-1', kind: 'skill', title: 'Organize customer questions', description: 'Summarize questions for review',
    requester_id: 'owner', requester_email: 'owner@example.com', requester_role: 'admin',
    risk_level: 'Low', scan: [], status: 'pending', stage: 'awaiting_admin',
    manager_by: null, admin_by: null, decided_reason: null, executed: false, execute_error: null,
    created_at: '2026-10-31T00:00:00Z', ttl_seconds: 3600, ...overrides,
  };
}

describe('<InstallDetailPanel>', () => {
  it('keeps decisions outside the scrolling details', () => {
    renderWithProviders(<InstallDetailPanel request={request()} onDecided={vi.fn()} />);
    const actions = screen.getByTestId('install-actions');
    expect(actions).toHaveClass('sticky', 'bottom-0', 'shrink-0');
    expect(actions.previousElementSibling).toHaveClass('overflow-y-auto');
    expect(screen.getByText(en['approval.risk.low'])).toBeInTheDocument();
  });

  it.each(['approved', 'denied'] as const)('shows a processed %s outcome with no controls', (outcome) => {
    renderWithProviders(<InstallDetailPanel request={request()} processed outcome={outcome} onDecided={vi.fn()} />);
    expect(screen.getByText(en[`inbox.status.${outcome}`])).toBeInTheDocument();
    expect(screen.queryByTestId('install-actions')).not.toBeInTheDocument();
  });

  it('shows a completed server request as read-only without local state', () => {
    renderWithProviders(<InstallDetailPanel request={request({ status: 'denied', stage: 'denied' })} onDecided={vi.fn()} />);
    expect(screen.getByText(en['inbox.status.denied'])).toBeInTheDocument();
    expect(screen.queryByTestId('install-actions')).not.toBeInTheDocument();
  });
});
