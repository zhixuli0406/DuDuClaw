import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import type { IsolationReadiness, SystemStatus } from '@/lib/api';
import { isVisible } from '@/lib/nav-visibility';
import { navGroups, navGroupsForEdition, personalAdvancedGroup, personalPrimaryItems } from '@/components/layout/nav-model';
import { ProtectedModePage } from './ProtectedModePage';
import { employeeList, routeCalls } from './test-helpers';

const components: IsolationReadiness['components'] = {
  launch_service: 'ready', sandbox: 'ready', network: 'ready', storage: 'ready',
  parse_worker: 'ready', update_service: 'ready', mcp_boundary: 'ready', runtime_adapters: 'ready',
};
function setStatus(over: Partial<SystemStatus>) {
  useSystemStore.setState({ status: { version: '1.81.0', uptime_seconds: 1, agents_count: 2, channels_connected: 0,
    gateway_address: 'localhost', ...over } });
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
});
afterEach(() => {
  useSystemStore.setState({ status: null });
});

describe('ProtectedModePage', () => {
  it('waits for the system status before deciding', () => {
    useSystemStore.setState({ status: null });
    renderWithProviders(<ProtectedModePage />);
    expect(screen.getByRole('status')).toHaveTextContent('Loading');
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });

  it('explains when the install is not in protected mode and calls nothing', () => {
    setStatus({ isolation_readiness: null });
    renderWithProviders(<ProtectedModePage />);
    expect(screen.getByText('Protected mode is not turned on here')).toBeInTheDocument();
    expect(screen.queryByRole('tab')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });

  it('tells Personal installs that protected mode is Enterprise only', () => {
    setStatus({ edition_profile: 'personal', isolation_readiness: { required: true, ready: true, components } });
    renderWithProviders(<ProtectedModePage />);
    expect(screen.getByText('Protected mode is available in the Enterprise edition only.')).toBeInTheDocument();
  });

  it('shows four tabs, names owners from the member list and switches tabs', async () => {
    setStatus({ isolation_readiness: { required: true, ready: true, components } });
    routeCalls({
      'users.list': () => ({ users: [{ id: 'u-alice', display_name: 'Alice Chen', email: 'alice@example.com',
        role: 'employee', status: 'active', created_at: '', updated_at: '', bindings: [] }] }),
      'isolation.employee_list': () => employeeList(),
      'security.audit_log': () => ({ events: [] }),
    });
    renderWithProviders(<ProtectedModePage />);
    expect(screen.getByRole('heading', { level: 1, name: 'Protected mode' })).toBeInTheDocument();
    const tabs = screen.getAllByRole('tab');
    expect(tabs.map((tab) => tab.textContent)).toEqual(['AI employees', 'Connector accounts', 'Google tenant source', 'Records']);
    expect(screen.getByRole('tab', { name: 'AI employees' })).toHaveAttribute('aria-selected', 'true');
    const employees = await screen.findByRole('region', { name: 'Protected AI employees' });
    expect(await within(employees).findByText('Alice Chen')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('tab', { name: 'Records' }));
    expect(await screen.findByText('No matching records')).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Records' })).toHaveAttribute('aria-selected', 'true');
  });

  it('keeps the readiness bar at the top of the page', async () => {
    setStatus({ isolation_readiness: { required: true, ready: false, components: { ...components, network: 'unavailable' } } });
    routeCalls({
      'users.list': () => ({ users: [] }),
      'isolation.employee_list': () => employeeList(),
    });
    renderWithProviders(<ProtectedModePage />);
    expect(screen.getAllByRole('alert')[0]).toHaveTextContent('Protected TEAM work is paused');
    await screen.findByRole('region', { name: 'Protected AI employees' });
  });

  it('links connector reasons in the readiness bar to the connector tab', async () => {
    useAuthStore.setState({ user: { id: 'admin', email: 'admin@test.invalid', display_name: 'Admin', role: 'admin', status: 'active' } });
    setStatus({ isolation_readiness: { required: true, ready: true, status: 'action_required', components,
      reasons: [{ code: 'connector_setup_required', provider: 'google' }] } });
    routeCalls({
      'users.list': () => ({ users: [] }),
      'isolation.employee_list': () => employeeList(),
      'isolation.connector_list': () => ({ ...employeeList(), routes: [], next: null }),
    });
    renderWithProviders(<ProtectedModePage />);
    const link = screen.getByRole('link', { name: 'Go to connector settings' });
    expect(link).toHaveAttribute('href', '/app/system/protected?tab=connectors');
    fireEvent.click(link);
    expect(await screen.findByRole('tab', { name: 'Connector accounts' })).toHaveAttribute('aria-selected', 'true');
    useAuthStore.setState({ user: null });
  });
});

describe('設定 › 受保護模式 navigation', () => {
  const entry = navGroups[2].items.find((item) => item.to === '/app/system/protected')!;

  it('is a versioned Admin-only Enterprise entry in the 設定 group', () => {
    expect(entry).toBeDefined();
    expect(entry.label).toBe('protectedMode.title');
    expect(entry.newIn).toBe('1.81.0');
    expect(entry.minRole).toBe('admin');
    expect(entry.enterprise).toBe(true);
  });

  it('shows only once the install reports protected mode', () => {
    expect(isVisible(entry, 'admin', false)).toBe(false);
    expect(isVisible(entry, 'admin', false, { protectedMode: false })).toBe(false);
    expect(isVisible(entry, 'admin', false, { protectedMode: true })).toBe(true);
    expect(isVisible(entry, 'manager', false, { protectedMode: true })).toBe(false);
    expect(isVisible(entry, 'employee', false, { protectedMode: true })).toBe(false);
  });

  it('never appears in the Personal edition lists', () => {
    expect(isVisible(entry, 'admin', true, { protectedMode: true })).toBe(false);
    const personal = [...navGroupsForEdition(true).flatMap((group) => group.items), ...personalPrimaryItems, ...personalAdvancedGroup.items];
    expect(personal.filter((item) => item.to === '/app/system/protected' && isVisible(item, 'admin', true, { protectedMode: true })))
      .toHaveLength(0);
  });
});
