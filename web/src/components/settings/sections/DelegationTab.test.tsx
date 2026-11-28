import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
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

vi.mock('./AdminAiCard', () => ({ AdminAiCard: () => null }));

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
  getMock.mockResolvedValue({ hash: 'delegation-1',
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
      expect(selects[2].value).toBe('warehouse-lead');
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
    expect(setMock.mock.calls[0][0]).toEqual({ expected_hash: 'delegation-1',
      policy: 'hierarchy',
      unrestricted_roles: [], allow_one_way: [],
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
    expect(setMock.mock.calls[0][0]).toEqual({ expected_hash: 'delegation-1', policy: 'department', allow: [], unrestricted_roles: [], allow_one_way: [] });
  });

  it('surfaces gateway config warnings', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1',
      policy: 'department',
      allow: [],
      warnings: [{ code: 'private-agent-123', message: '<b>Some pairs were ignored.</b>' }, 'config.toml [delegation] allow'],
    });
    renderWithProviders(<DelegationTab />);
    expect(await screen.findByText('<b>Some pairs were ignored.</b>')).toBeInTheDocument();
    expect(screen.getByText(en['settings.delegation.safeWarning'])).toBeInTheDocument();
    expect(document.querySelector('b')).toBeNull();
    expect(document.body.textContent).not.toContain('[delegation] allow');
    expect(document.body.textContent).not.toContain('private-agent-123');
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
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'hierarchy', allow: [], warnings: [] });
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
    expect(setMock.mock.calls[0][0]).toEqual({ expected_hash: 'delegation-1', policy: 'hierarchy', allow: [], unrestricted_roles: [], allow_one_way: [] });
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
    expect(setMock.mock.calls[0][0]).toEqual({ expected_hash: 'delegation-1',
      policy: 'none',
      unrestricted_roles: [], allow_one_way: [],
      allow: [['sales-lead', 'warehouse-lead']],
    });
  });

  it('shows a loaded "none" policy as selected', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'none', configured_policy: 'none', allow: [], warnings: [] });
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


describe('delegation role and direction settings', () => {
  it('loads checked roles, adds a role and saves both lists', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'none', allow: [], unrestricted_roles: ['manager'], allow_one_way: [], warnings: [] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    expect(screen.getAllByRole('checkbox')).toHaveLength(2);
    expect(screen.getByRole('checkbox', { name: 'Manager' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Administrator' })).not.toBeChecked();
    setMock.mockImplementationOnce(async (params) => {
      getMock.mockResolvedValue({ hash: 'delegation-1', ...params, warnings: [] });
      return { success: true, ...params, applied: true };
    });
    fireEvent.click(screen.getByRole('checkbox', { name: 'Administrator' }));
    expect(screen.getByRole('alert', { hidden: true })).toHaveTextContent(en['settings.delegation.roles.noneWarning']);
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('button', { name: 'Select' })).toBeDisabled();
    expect(setMock).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole('checkbox'));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Select' }));
    expect(saveButton()).not.toBeDisabled();
    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalledWith({ expected_hash: 'delegation-1', policy: 'none', allow: [], unrestricted_roles: ['manager', 'admin'], allow_one_way: [], acknowledge_risk: true }));
    await waitLoaded();
    expect(screen.getByRole('checkbox', { name: 'Manager' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Administrator' })).toBeChecked();
  });

  it('removes a saved role without acknowledgement', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'none', allow: [], unrestricted_roles: ['manager'], allow_one_way: [], warnings: [] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Manager' }));
    expect(screen.queryByText(en['settings.delegation.roles.risk'])).not.toBeInTheDocument();
    expect(screen.getAllByRole('checkbox')).toHaveLength(2);
    expect(saveButton()).not.toBeDisabled();
    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalledWith({ expected_hash: 'delegation-1', policy: 'none', allow: [], unrestricted_roles: [], allow_one_way: [] }));
  });

  it('cancelling a role confirmation leaves the role unticked', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Administrator' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: 'Administrator' })).not.toBeChecked();
    expect(setMock).not.toHaveBeenCalled();
  });

  it('requires confirmation for each newly selected role, but never on untick', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    for (const role of ['Administrator', 'Manager']) {
      fireEvent.click(screen.getByRole('checkbox', { name: role }));
      const dialog = screen.getByRole('dialog');
      expect(within(dialog).getByRole('button', { name: 'Select' })).toBeDisabled();
      fireEvent.click(within(dialog).getByRole('checkbox'));
      fireEvent.click(within(dialog).getByRole('button', { name: 'Select' }));
      expect(screen.getByRole('checkbox', { name: role })).toBeChecked();
      fireEvent.click(screen.getByRole('checkbox', { name: role }));
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    }
  });

  it('explains one-way delegation using display names', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'department', allow: [], allow_one_way: [['intern', 'sales-lead']] });
    renderWithProviders(<DelegationTab />);
    expect(await screen.findByText('Intern can delegate work to Sales lead; the reverse is not allowed.')).toBeInTheDocument();
    expect(screen.queryByText(/intern can delegate/)).not.toBeInTheDocument();
  });

  it('blocks more than 200 pairs counting both directions', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'department', allow: Array.from({ length: 200 }, (_, i) => [`a${i}`, `b${i}`]), allow_one_way: [['intern', 'sales-lead']] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    expect(screen.getByRole('alert')).toHaveTextContent(en['settings.delegation.pairs.limit']);
    expect(saveButton()).toBeDisabled();
    fireEvent.click(saveButton());
    expect(setMock).not.toHaveBeenCalled();
  }, 20_000);

  it.each([
    { allow: [['sales-lead', 'sales-lead']], allow_one_way: [] },
    { allow: [['sales-lead', 'warehouse-lead']], allow_one_way: [['warehouse-lead', 'sales-lead']] },
    { allow: [], allow_one_way: [['sales-lead', 'warehouse-lead'], ['sales-lead', 'warehouse-lead']] },
  ])('marks conflicting rows and blocks self or duplicate pairs: %j', async (pairs) => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'department', ...pairs });
    const { container } = renderWithProviders(<DelegationTab />);
    await waitLoaded();
    expect(saveButton()).toBeDisabled();
    for (const row of container.querySelectorAll('[data-pair-row]')) expect(row).toHaveClass('ring-destructive');
  });

  it('keeps opposite one-way pairs distinct', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'department', allow: [], allow_one_way: [['sales-lead', 'warehouse-lead'], ['warehouse-lead', 'sales-lead']] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    expect(saveButton()).not.toBeDisabled();
  });

  it('shows the server refusal above Save without marking an unnamed row', async () => {
    const refusal = '單向配對與雙向配對重複，請移除其中一組。';
    setMock.mockRejectedValueOnce(new Error(refusal));
    const { container } = renderWithProviders(<DelegationTab />);
    await waitLoaded();
    fireEvent.click(saveButton());
    const error = await screen.findByRole('alert');
    expect(error).toHaveTextContent(refusal);
    expect(error.compareDocumentPosition(saveButton()) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(container.querySelector('[data-pair-row].ring-destructive')).toBeNull();
  });

  it('saves a changed direction and swaps an existing one-way row', async () => {
    getMock.mockResolvedValue({ hash: 'delegation-1', policy: 'department', allow: [['sales-lead', 'warehouse-lead']], unrestricted_roles: [], allow_one_way: [['intern', 'sales-lead']], warnings: [] });
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    fireEvent.change(screen.getAllByRole('combobox')[1], { target: { value: 'one_way' } });
    fireEvent.click(screen.getAllByRole('button', { name: 'Swap direction' })[1]);
    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalledWith({ expected_hash: 'delegation-1', policy: 'department', allow: [], unrestricted_roles: [], allow_one_way: [['sales-lead', 'warehouse-lead'], ['sales-lead', 'intern']] }));
  });
});

describe('delegation compare-and-swap', () => {
  it.each([
    '設定已被其他人變更，請重新整理後再儲存。',
    '設定頁的資料已過期，請重新整理後再儲存。',
  ])('locks editing on server refusal: %s', async message => {
    setMock.mockRejectedValueOnce(new Error(message));
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    fireEvent.click(saveButton());
    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(setMock).toHaveBeenCalledWith(expect.objectContaining({ expected_hash: 'delegation-1' }));
    expect(getMock).toHaveBeenCalledTimes(1);
    expect(saveButton()).toBeDisabled();
    expect(screen.getByRole('radio', { name: /Department mode/i })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Reload' }));
    await waitLoaded();
    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenCalledTimes(2));
  });
  it('only reloads after the set and uses the refreshed hash', async () => {
    renderWithProviders(<DelegationTab />);
    await waitLoaded();
    getMock.mockResolvedValue({ hash: 'delegation-2', policy: 'department', allow: [], warnings: [] });
    fireEvent.click(saveButton());
    await waitFor(() => expect(getMock).toHaveBeenCalledTimes(2));
    await waitLoaded();
    expect(setMock.mock.invocationCallOrder[0]).toBeLessThan(getMock.mock.invocationCallOrder[1]);
    fireEvent.click(saveButton());
    await waitFor(() => expect(setMock).toHaveBeenLastCalledWith(expect.objectContaining({ expected_hash: 'delegation-2' })));
  });
  it('cannot save a response without a hash', async () => {
    getMock.mockResolvedValue({ policy: 'department', allow: [], warnings: [] });
    renderWithProviders(<DelegationTab />);
    await waitFor(() => expect(getMock).toHaveBeenCalledOnce());
    expect(saveButton()).toBeDisabled();
    fireEvent.click(saveButton());
    expect(setMock).not.toHaveBeenCalled();
  });
});
