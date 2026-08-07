import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore, type UserRole } from '@/stores/auth-store';

// DelegationTab reads the policy + whitelist on mount and writes both back in
// one delegation.set call. The agent roster feeds the pair pickers.
const getMock = vi.fn();
const setMock = vi.fn();
const agentsListMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    delegation: {
      get: () => getMock(),
      set: (params: Record<string, unknown>) => setMock(params),
    },
    agents: {
      list: () => agentsListMock(),
    },
  },
}));

import { DelegationTab } from './DelegationTab';

function signIn(role: UserRole) {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u1@test.local', display_name: 'U1', role, status: 'active' },
  } as never);
}

beforeEach(() => {
  vi.clearAllMocks();
  getMock.mockResolvedValue({
    policy: 'department',
    allow: [['sales-lead', 'warehouse-lead']],
    warnings: [],
  });
  setMock.mockResolvedValue({ success: true, policy: 'department', allow: [], applied: true });
  agentsListMock.mockResolvedValue({
    agents: [
      { name: 'sales-lead', display_name: 'Sales lead', department: 'Sales' },
      { name: 'warehouse-lead', display_name: 'Warehouse lead', department: 'Warehouse' },
      { name: 'intern', display_name: 'Intern', department: '' },
    ],
  });
  signIn('admin');
});

describe('<DelegationTab>', () => {
  it('renders the saved policy and pair for an admin', async () => {
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());

    const department = screen.getByRole('radio', { name: /Department mode/i }) as HTMLInputElement;
    expect(department.checked).toBe(true);
    // Both sides of the seeded pair are selected, with their department badges.
    await waitFor(() => {
      const selects = screen.getAllByRole('combobox') as HTMLSelectElement[];
      expect(selects[0].value).toBe('sales-lead');
      expect(selects[1].value).toBe('warehouse-lead');
    });
    expect(screen.getByText('Sales')).toBeInTheDocument();
    expect(screen.getByText('Warehouse')).toBeInTheDocument();
  });

  it('renders nothing for a non-admin and never calls the RPC', async () => {
    signIn('manager');
    const { container } = renderWithProviders(<DelegationTab />);
    expect(container).toBeEmptyDOMElement();
    expect(getMock).not.toHaveBeenCalled();
  });

  it('warns about open mode instead of silently widening access', async () => {
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());

    fireEvent.click(screen.getByRole('radio', { name: /Open mode/i }));
    expect(screen.getByText(/give work orders to anyone else/i)).toBeInTheDocument();
  });

  it('sends the edited policy and pairs in one call', async () => {
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());

    fireEvent.click(screen.getByRole('radio', { name: /Reporting-line mode/i }));
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(setMock).toHaveBeenCalled());
    expect(setMock.mock.calls[0][0]).toEqual({
      policy: 'hierarchy',
      allow: [['sales-lead', 'warehouse-lead']],
    });
  });

  it('blocks saving while a pair is half-filled', async () => {
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());

    fireEvent.click(screen.getByRole('button', { name: /Add a pair/i }));
    const save = screen.getByRole('button', { name: /^Save$/i }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(screen.getByText(/needs two AI staff members/i)).toBeInTheDocument();

    fireEvent.click(save);
    expect(setMock).not.toHaveBeenCalled();
  });

  it('removes a pair from the payload when its delete button is clicked', async () => {
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());

    fireEvent.click(screen.getByLabelText('Remove the pair Sales lead and Warehouse lead'));
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(setMock).toHaveBeenCalled());
    expect(setMock.mock.calls[0][0]).toEqual({ policy: 'department', allow: [] });
  });

  it('surfaces gateway config warnings', async () => {
    getMock.mockResolvedValue({
      policy: 'department',
      allow: [],
      warnings: ['config.toml [delegation] allow 第 2 組設定格式有誤,已忽略。'],
    });
    renderWithProviders(<DelegationTab />);
    await waitFor(() =>
      expect(screen.getByText(/第 2 組設定格式有誤/)).toBeInTheDocument()
    );
  });
});
