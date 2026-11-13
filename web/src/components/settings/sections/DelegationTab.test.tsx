import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';

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

import { DelegationTab, DELEGATION_POLICIES } from './DelegationTab';

function signIn(role: UserRole) {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u1@test.local', display_name: 'U1', role, status: 'active' },
  } as never);
}

/** The form is usable only after `delegation.get` succeeded (RM-137). */
async function waitLoaded() {
  await waitFor(() =>
    expect(screen.getByRole('radio', { name: /Department mode/i })).not.toBeDisabled(),
  );
}

const saveButton = () => screen.getByRole('button', { name: /^Save$/i }) as HTMLButtonElement;

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
    await waitLoaded();

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
    await waitLoaded();

    fireEvent.click(screen.getByRole('radio', { name: /Open mode/i }));
    expect(screen.getByText(/give work orders to anyone else/i)).toBeInTheDocument();
  });

  it('sends the edited policy and pairs in one call', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();

    fireEvent.click(screen.getByRole('radio', { name: /Reporting-line mode/i }));
    fireEvent.click(saveButton());

    await waitFor(() => expect(setMock).toHaveBeenCalled());
    expect(setMock.mock.calls[0][0]).toEqual({
      policy: 'hierarchy',
      allow: [['sales-lead', 'warehouse-lead']],
    });
  });

  it('blocks saving while a pair is half-filled', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();

    fireEvent.click(screen.getByRole('button', { name: /Add a pair/i }));
    const save = saveButton();
    expect(save.disabled).toBe(true);
    expect(screen.getByText(/needs two AI staff members/i)).toBeInTheDocument();

    fireEvent.click(save);
    expect(setMock).not.toHaveBeenCalled();
  });

  it('removes a pair from the payload when its delete button is clicked', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();

    fireEvent.click(screen.getByLabelText('Remove the pair Sales lead and Warehouse lead'));
    fireEvent.click(saveButton());

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

  // ── RM-137 ────────────────────────────────────────────────────────────

  it('cannot save after a failed load and never submits the placeholder defaults', async () => {
    getMock.mockRejectedValue(new Error('config.toml unreadable'));
    renderWithProviders(<DelegationTab />);

    expect(await screen.findByText(en['errorState.manage.loadFailed'])).toBeInTheDocument();
    expect(screen.getByText(en['settings.delegation.saveBlocked'])).toBeInTheDocument();
    // No radio claims to be the setting, and none can be picked.
    for (const radio of screen.getAllByRole('radio') as HTMLInputElement[]) {
      expect(radio.checked).toBe(false);
      expect(radio).toBeDisabled();
    }
    expect(screen.queryAllByRole('combobox')).toHaveLength(0);
    expect(screen.getByRole('button', { name: /Add a pair/i })).toBeDisabled();
    const save = saveButton();
    expect(save.disabled).toBe(true);
    fireEvent.click(save);
    expect(setMock).not.toHaveBeenCalled();
  });

  it('enables saving only once a retry has loaded the real setting', async () => {
    getMock.mockRejectedValueOnce(new Error('timeout'));
    getMock.mockResolvedValueOnce({ policy: 'hierarchy', allow: [], warnings: [] });
    renderWithProviders(<DelegationTab />);

    expect(await screen.findByText(en['errorState.manage.loadFailed'])).toBeInTheDocument();
    expect(saveButton().disabled).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: en['errorState.retry'] }));
    await waitLoaded();
    expect((screen.getByRole('radio', { name: /Reporting-line mode/i }) as HTMLInputElement).checked).toBe(true);
    expect(saveButton().disabled).toBe(false);

    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalled());
    // Exactly what was loaded — not `department` + no pairs.
    expect(setMock.mock.calls[0][0]).toEqual({ policy: 'hierarchy', allow: [] });
  });

  it('offers "none" with a plain description and saves it', async () => {
    expect(DELEGATION_POLICIES).toContain('none');
    renderWithProviders(<DelegationTab />);
    await waitLoaded();

    const none = screen.getByRole('radio', { name: new RegExp(en['settings.delegation.policy.none']) });
    expect(screen.getByText(en['settings.delegation.policy.none.help'])).toBeInTheDocument();
    fireEvent.click(none);
    expect(screen.getByText(en['settings.delegation.policy.none.note'])).toBeInTheDocument();
    // The saved pair is still shown, marked as not applying under `none`.
    expect(screen.getByText(en['settings.delegation.pairs.inactiveUnderNone'])).toBeInTheDocument();

    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalled());
    expect(setMock.mock.calls[0][0]).toEqual({
      policy: 'none',
      allow: [['sales-lead', 'warehouse-lead']],
    });
  });

  it('shows a loaded "none" policy as selected', async () => {
    getMock.mockResolvedValue({ policy: 'none', configured_policy: 'none', allow: [], warnings: [] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    expect(
      (screen.getByRole('radio', { name: new RegExp(en['settings.delegation.policy.none']) }) as HTMLInputElement).checked,
    ).toBe(true);
  });

  it('has the new strings in all three catalogues', () => {
    for (const key of [
      'settings.delegation.policy.none',
      'settings.delegation.policy.none.help',
      'settings.delegation.policy.none.note',
      'settings.delegation.pairs.inactiveUnderNone',
      'settings.delegation.notLoaded',
      'settings.delegation.saveBlocked',
    ]) {
      for (const catalogue of [en, zhTW, jaJP] as Array<Record<string, string>>) {
        expect(catalogue[key], key).toBeTruthy();
      }
    }
  });
});
