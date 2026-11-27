import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { BillingPage } from './BillingPage';
import { useConnectionStore } from '@/stores/connection-store';

import { useAuthStore } from '@/stores/auth-store';

beforeEach(() => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never, deptGrants: [], deptGroups: new Set() });
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // The page tolerates missing fields — a benign object keeps every optional
  // chain safe (usage?.x.used ?? 0 / budget.incidents → {}).
  mockWsClient.call.mockResolvedValue({});
});

describe('BillingPage', () => {
  it('renders the billing header title', () => {
    renderWithProviders(<BillingPage />);
    expect(screen.getByText('Billing')).toBeInTheDocument();
  });
});


it('shows department usage only for a non-admin with a cost grant', () => {
  useAuthStore.setState({ deptGrants: [{ department: 'Sales', groups: ['schedules'] }] });
  const view = renderWithProviders(<BillingPage />);
  expect(screen.queryByText('Department usage')).not.toBeInTheDocument();
  view.unmount();
  useAuthStore.setState({ user: { role: 'admin' } as never, deptGrants: [{ department: 'Sales', groups: ['cost'] }] });
  renderWithProviders(<BillingPage />);
  expect(screen.queryByText('Department usage')).not.toBeInTheDocument();
});

it('renders granted department employees, totals and recent usage with millicent conversion', async () => {
  useAuthStore.setState({ deptGrants: [{ department: 'Sales', groups: ['cost'] }, { department: 'Support', groups: ['skills'] }] });
  const totals = { total_requests: 4, total_input_tokens: 100, total_output_tokens: 50,
    total_cost_millicents: 123000, total_cache_savings_millicents: 20000, avg_cache_efficiency: 0.5 };
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
    method === 'cost.department_summary' ? { available: true, agents: [{ agent_id: 'agnes', display_name: 'Agnes', ...totals }], totals }
      : method === 'cost.department_recent' ? { available: true, records: [{ agent_id: 'agnes', model: 'test-model', created_at: '2026-11-27T00:00:00Z', cost_millicents: 1000 }] } : {}));
  renderWithProviders(<BillingPage />);
  expect(screen.getByText('Department usage')).toBeInTheDocument();
  const department = within(screen.getByRole('region', { name: 'Department usage' }));
  expect(within(await department.findByRole('table')).getByText('Agnes')).toBeInTheDocument();
  expect(screen.getByText('Total')).toBeInTheDocument();
  expect(screen.getAllByText('$1.23')).toHaveLength(2);
  expect(screen.getByText('test-model')).toBeInTheDocument();
  expect(screen.queryByRole('option', { name: 'Support' })).not.toBeInTheDocument();
  fireEvent.change(screen.getByRole('combobox', { name: 'Period' }), { target: { value: '90' } });
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('cost.department_summary', { department: 'Sales', days: 90 }));
});


it.each([false, true])('shows plain-language unavailable or empty department usage (%s)', async available => {
  useAuthStore.setState({ deptGrants: [{ department: 'Sales', groups: ['cost'] }] });
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
    method === 'cost.department_summary' ? { available, agents: [] }
      : method === 'cost.department_recent' ? { available, records: [] } : {}));
  renderWithProviders(<BillingPage />);
  if (available) {
    await screen.findByText('No usage was recorded for this department during the selected period.');
    expect(screen.getByText('This department has no recent usage records.')).toBeInTheDocument();
  } else {
    await waitFor(() => expect(screen.getAllByText('Usage details for this department are currently unavailable. Please try again later.')).toHaveLength(2));
  }
});
