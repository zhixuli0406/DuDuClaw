import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { EmployeeHealthPage } from './EmployeeHealthPage';
import { loadEmployeeHealth, type EmployeeHealthSnapshot } from '@/lib/employee-health';

vi.mock('@/lib/employee-health', async importOriginal => ({
  ...await importOriginal<typeof import('@/lib/employee-health')>(),
  loadEmployeeHealth: vi.fn(),
}));

function fixture(): EmployeeHealthSnapshot {
  return { window_days: 1, partial: false, agents: [{
    id: 'alice', name: 'Alice', last_success_at: '2026-11-04T10:30:00Z',
    failures: { empty_reply: 2, quota: 1, rate_limited: 0, auth_dead: 0, timeout: 0, gate_refused: 0, delivery: 0, other: 0 },
    consecutive_failures: 0, paused_goals: 0, account_status: 'available',
  }] };
}
const renderPage = () => renderWithProviders(<SidebarProvider><EmployeeHealthPage /></SidebarProvider>);
beforeEach(() => { vi.clearAllMocks(); vi.mocked(loadEmployeeHealth).mockResolvedValue(fixture()); });

describe('EmployeeHealthPage', () => {
  it('shows a summary and keeps failure details behind disclosure', async () => {
    renderPage();
    await screen.findByText('Alice');
    expect(screen.getByText(/Generating a reply does not confirm delivery/)).toBeVisible();
    expect(loadEmployeeHealth).toHaveBeenCalledWith(1);
    expect(screen.getByRole('region', { name: 'Employee health summary' })).toHaveTextContent('Replying normally');
    expect(screen.getByText('Account and failure details').closest('details')).not.toHaveAttribute('open');
    fireEvent.click(screen.getByText('Account and failure details'));
    expect(screen.getByText('Account and failure details').closest('details')).toHaveAttribute('open');
    expect(screen.getByText('No reply produced')).toBeVisible();
    expect(screen.getByRole('link', { name: 'Edit in employee settings →' })).toHaveAttribute('href', '/agents/alice/edit');
  });

  it('switches the reporting window and warns about incomplete records', async () => {
    vi.mocked(loadEmployeeHealth).mockResolvedValue({ ...fixture(), partial: true });
    renderPage();
    await screen.findByText(/Some records could not be read/);
    fireEvent.click(screen.getByRole('radio', { name: 'Last 7 days' }));
    await waitFor(() => expect(loadEmployeeHealth).toHaveBeenLastCalledWith(7));
  });

  it('does not label missing reply history as healthy', async () => {
    const snapshot = fixture();
    snapshot.agents[0].last_success_at = null;
    vi.mocked(loadEmployeeHealth).mockResolvedValue(snapshot);
    renderPage();
    await screen.findByText('No generated reply recorded');
    expect(screen.getByText('Alice').closest('[data-slot="card"]')).toHaveTextContent('Not enough data');
  });

  it('renders an empty state and recovers from a failed refresh', async () => {
    vi.mocked(loadEmployeeHealth).mockRejectedValueOnce(new Error('Request failed'));
    renderPage();
    await screen.findByRole('alert');
    vi.mocked(loadEmployeeHealth).mockResolvedValue({ ...fixture(), agents: [] });
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await screen.findByText('No employees yet');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
