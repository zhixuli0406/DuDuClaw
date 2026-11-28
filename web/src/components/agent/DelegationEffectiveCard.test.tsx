import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore, type UserRole } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';

const effectiveMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: { delegation: { effective: (id: string) => effectiveMock(id) } },
}));

import { DelegationEffectiveCard, isDelegationEffective } from './DelegationEffectiveCard';

const BASE = {
  agent_id: 'sales-rep',
  policy: 'department',
  source: 'global',
  global_policy: 'department',
  configured_policy: 'department',
  policy_warning: null,
  config_error: null,
  allow_warning_count: 0,
  may_delegate: true,
  may_create_agents: false,
  permissions: { can_send_cross_agent: true, can_create_agents: false },
  allow_pairs: [['sales-rep', 'mkt-lead']],
  inactive_allow_pairs: [],
};

function signIn(role: UserRole) {
  useAuthStore.setState({ user: { id: 'u1', role, status: 'active' } as never, bindings: [] });
}

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ agents: [{ name: 'sales-rep', display_name: 'Sales person' }, { name: 'mkt-lead', display_name: 'Marketing lead' }] as never });
  effectiveMock.mockResolvedValue(BASE);
  useConnectionStore.setState({ state: 'authenticated' } as never);
  signIn('manager');
});

describe('DelegationEffectiveCard (RM-137)', () => {
  it('shows the effective policy, its source and the pairs naming the employee', async () => {
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByText(en['settings.delegation.policy.department'])).toBeInTheDocument();
    expect(effectiveMock).toHaveBeenCalledWith('sales-rep');
    expect(screen.getByText(en['agentDetail.delegation.mayDelegate'])).toBeInTheDocument();
    expect(screen.getByText(en['agentDetail.delegation.source.global'])).toBeInTheDocument();
    expect(screen.getByText(en['agentDetail.delegation.mayNotCreate'])).toBeInTheDocument();
    expect(screen.getByText(/Sales person ⇄ Marketing lead/)).toBeInTheDocument();
  });

  it('explains a `none` that comes from the employee switch', async () => {
    effectiveMock.mockResolvedValue({
      ...BASE, policy: 'none', source: 'agent_permissions', may_delegate: false,
      allow_pairs: [], inactive_allow_pairs: [['sales-rep', 'mkt-lead']],
      permissions: { can_send_cross_agent: false, can_create_agents: false },
    });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByText(en['settings.delegation.policy.none'])).toBeInTheDocument();
    expect(screen.getByText(en['agentDetail.delegation.mayNotDelegate'])).toBeInTheDocument();
    expect(screen.getByText(en['agentDetail.delegation.source.agent_permissions'])).toBeInTheDocument();
    expect(screen.getByText(/do not apply in the current mode: Sales person ⇄ Marketing lead/)).toBeInTheDocument();
    expect(screen.getByText(en['agentDetail.delegation.teamNote'])).toBeInTheDocument();
  });

  it('renders a masked pair peer as "another employee you cannot view" (RM-137 L1)', async () => {
    effectiveMock.mockResolvedValue({
      ...BASE,
      allow_pairs: [['sales-rep', 'mkt-lead'], ['*hidden*', 'sales-rep']],
      hidden_allow_pairs: 1,
      hidden_inactive_allow_pairs: 0,
      hidden_peer_placeholder: '*hidden*',
    });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByText(new RegExp(`${en['agentDetail.delegation.hiddenPeer']} ⇄ Sales person`)))
      .toBeInTheDocument();
    expect(screen.queryByText(/\*hidden\*/)).not.toBeInTheDocument();
    expect(zhTW['agentDetail.delegation.hiddenPeer' as keyof typeof zhTW]).toBeTruthy();
    expect(jaJP['agentDetail.delegation.hiddenPeer' as keyof typeof jaJP]).toBeTruthy();
  });

  it('surfaces config_error as an alert with the fixed message for its category', async () => {
    for (const kind of ['unreadable', 'unparsable', 'permissions_unreadable', 'unknown'] as const) {
      effectiveMock.mockResolvedValue({
        ...BASE, policy: 'none', global_policy: 'none', may_delegate: false, allow_pairs: [],
        config_error: kind,
      });
      const { unmount } = renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
      const alert = await screen.findByRole('alert');
      expect(alert).toHaveTextContent((en as Record<string, string>)[`agentDetail.delegation.configError.${kind}`]);
      unmount();
    }
  });

  it('shows an unknown owner role as one muted line without a config alert', async () => {
    effectiveMock.mockResolvedValue({
      ...BASE, unrestricted: false, owner_role: null, unrestricted_error: 'users_unreadable',
    });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    const note = await screen.findByText(en['agentDetail.delegation.unrestrictedUnknown']);
    expect(note).toHaveClass('text-muted-foreground');
    expect(screen.getAllByText(en['agentDetail.delegation.unrestrictedUnknown'])).toHaveLength(1);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.queryByText(/No delegation scope limits/)).not.toBeInTheDocument();
  });

  it('never renders config text, even from an older gateway or in the Admin detail (RM-137 re-review)', async () => {
    signIn('admin');
    // An older gateway sent the raw parse error as `config_error`; an Admin
    // now also receives `config_error_detail`. Neither text is rendered.
    effectiveMock.mockResolvedValue({
      ...BASE, policy: 'none', global_policy: 'none', may_delegate: false, allow_pairs: [],
      config_error: 'config.toml 不是有效的 TOML:SECRET-CONFIG-MARKER',
      config_error_detail: 'config.toml 不是有效的 TOML:SECRET-CONFIG-MARKER',
    });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(en['agentDetail.delegation.configError.unknown']);
    expect(screen.queryByText(/SECRET-CONFIG-MARKER/)).not.toBeInTheDocument();
  });

  it('shows only the count of allow warnings to a Manager, never their text (RM-137 re-review L)', async () => {
    // What the gateway sends a Manager: the count, no texts.
    effectiveMock.mockResolvedValue({ ...BASE, allow_warning_count: 2 });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByText(/2 delegation setting warnings — an administrator can review them in Settings\./))
      .toBeInTheDocument();
    expect(screen.queryByText(/原始值/)).not.toBeInTheDocument();
    expect(zhTW['agentDetail.delegation.allowWarnings' as keyof typeof zhTW]).toContain('{count}');
    expect(jaJP['agentDetail.delegation.allowWarnings' as keyof typeof jaJP]).toContain('{count}');
  });

  it('never renders allow warning texts, even when a gateway sends them (Admin)', async () => {
    signIn('admin');
    effectiveMock.mockResolvedValue({
      ...BASE,
      allow_warning_count: 1,
      allow_warnings: ['config.toml [delegation] allow 項目已忽略:忽略自我配對("private-bot" 與自己)(原始值:["private-bot", "private-bot"])。'],
    });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByText(/1 delegation setting warning — an administrator/)).toBeInTheDocument();
    expect(screen.queryByText(/private-bot/)).not.toBeInTheDocument();
  });

  it('shows no warning line when there are none (or an older gateway omits the count)', async () => {
    const older: Record<string, unknown> = { ...BASE };
    delete older.allow_warning_count;
    for (const payload of [BASE, older]) {
      effectiveMock.mockResolvedValue(payload);
      const { unmount } = renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
      expect(await screen.findByText(en['settings.delegation.policy.department'])).toBeInTheDocument();
      expect(screen.queryByText(/delegation setting warning/)).not.toBeInTheDocument();
      unmount();
    }
    expect(isDelegationEffective({ ...BASE, allow_warning_count: 'two' })).toBe(false);
  });

  it('shows an error, never a guessed policy, for a refused or malformed answer', async () => {
    effectiveMock.mockResolvedValue({ name: 'sales-rep' });
    renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByText(en['settings.delegation.policy.department'])).not.toBeInTheDocument();
    expect(isDelegationEffective(BASE)).toBe(true);
    expect(isDelegationEffective({ ...BASE, policy: 'anarchy' })).toBe(false);
  });

  it('renders nothing below Manager and never calls the RPC', () => {
    signIn('employee');
    const { container } = renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
    expect(container).toBeEmptyDOMElement();
    expect(effectiveMock).not.toHaveBeenCalled();
  });

  it('has its strings in all three catalogues', () => {
    const keys = Object.keys(en).filter((k) => k.startsWith('agentDetail.delegation.'));
    expect(keys.length).toBeGreaterThan(10);
    for (const key of keys) {
      for (const catalogue of [zhTW, jaJP] as Array<Record<string, string>>) {
        expect(catalogue[key], key).toBeTruthy();
      }
    }
  });
});


it('shows unrestricted ownership and directional peers without exposing hidden ids', async () => {
  effectiveMock.mockResolvedValue({ ...BASE, unrestricted: true, owner_role: 'admin', allow_one_way: [
    { from: 'sales-rep', to: '*hidden*', direction: 'outgoing' },
    { from: 'mkt-lead', to: 'sales-rep', direction: 'incoming' },
  ] });
  renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  expect(await screen.findByText(/No delegation scope limits/)).toBeInTheDocument();
  expect(screen.getByText(/Can delegate to:/)).toHaveTextContent(en['agentDetail.delegation.hiddenPeer']);
  expect(screen.getByText(/Can accept work from:/)).toHaveTextContent('Marketing lead');
  expect(screen.queryByText(/\*hidden\*/)).not.toBeInTheDocument();
});


it('uses safe fallbacks for unknown ids and badges for direction and inactivity', async () => {
  effectiveMock.mockResolvedValue({ ...BASE, allow_pairs: [['sales-rep', 'unknown-id']], inactive_allow_one_way: [
    { from: 'sales-rep', to: 'other-unknown-id', direction: 'outgoing' },
  ] });
  const { container } = renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  await screen.findByText(/Can delegate to:/);
  expect(container).not.toHaveTextContent('unknown-id');
  expect(container).not.toHaveTextContent('sales-rep');
  expect(screen.getAllByText(new RegExp(en['agentDetail.delegation.unknownPeer']))).toHaveLength(2);
  expect(screen.getByText(en['agentDetail.delegation.oneWay'])).toHaveAttribute('data-slot', 'badge');
  expect(screen.getByText(en['agentDetail.delegation.oneWayInactive'])).toHaveAttribute('data-slot', 'badge');
  expect(container).not.toHaveTextContent('→');
});

it.each(['manager', 'admin'] as const)('shows safe policy warnings to %s without config or identifiers', async role => {
  signIn(role);
  effectiveMock.mockResolvedValue({ ...BASE, policy_warning: '[delegation] allow private-agent-123' });
  renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  expect(await screen.findByRole('alert')).toHaveTextContent(en['settings.delegation.safeWarning']);
  expect(document.body.textContent).not.toContain('[delegation] allow');
  expect(document.body.textContent).not.toContain('private-agent-123');
});

it.each([true, false])('shows the named avatar header when unrestricted=%s and the owner role is present', async unrestricted => {
  effectiveMock.mockResolvedValue({ ...BASE, unrestricted, owner_role: 'admin', owner_display_name: '王小明' });
  renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  expect(await screen.findByText("王小明’s AI avatar")).toBeInTheDocument();
});

it.each([null, '', '   '])('keeps the existing header when the owner name is %j', async owner_display_name => {
  effectiveMock.mockResolvedValue({ ...BASE, unrestricted: true, owner_role: 'admin', owner_display_name });
  renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  expect(await screen.findByText(/No delegation scope limits/)).toBeInTheDocument();
  expect(screen.queryByText(/’s AI avatar$/)).not.toBeInTheDocument();
});

it('does not label an ordinary employee as a named avatar', async () => {
  effectiveMock.mockResolvedValue({ ...BASE, owner_display_name: '王小明' });
  renderWithProviders(<DelegationEffectiveCard agentId="sales-rep" />);
  await screen.findByText(en['settings.delegation.policy.department']);
  expect(screen.queryByText(/’s AI avatar$/)).not.toBeInTheDocument();
});
