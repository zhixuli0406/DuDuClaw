import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import type { IsolationReadiness, SystemStatus } from '@/lib/api';
import { IsolationReadinessBanner } from './IsolationReadinessBanner';

const refresh = vi.fn(async () => {});
const originalRefresh = useSystemStore.getState().fetchStatus;
const readiness = (): IsolationReadiness => ({ required: true, ready: false, components: {
  launch_service: 'ready', sandbox: 'ready', network: 'ready', storage: 'ready',
  parse_worker: 'pending_integration', update_service: 'pending_integration',
  mcp_boundary: 'pending_integration', runtime_adapters: 'pending_integration',
} });
function setReadiness(value: IsolationReadiness | null | undefined) {
  const status: SystemStatus = { version: '1.80.0', uptime_seconds: 1, agents_count: 0,
    channels_connected: 0, gateway_address: 'localhost', isolation_readiness: value };
  useSystemStore.setState({ status, loading: false, fetchStatus: refresh });
}

beforeEach(() => {
  refresh.mockClear();
  useAuthStore.setState({ user: { id: 'admin', email: 'admin@test.invalid',
    display_name: 'Admin', role: 'admin', status: 'active' } });
  setReadiness(readiness());
});
afterEach(() => {
  useAuthStore.setState({ user: null });
  useSystemStore.setState({ status: null, loading: false, fetchStatus: originalRefresh });
});

describe('IsolationReadinessBanner', () => {
  it.each([undefined, null])('hides on installations without composite evidence: %s', value => {
    setReadiness(value);
    const { container } = renderWithProviders(<IsolationReadinessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it.each([{ ...readiness(), required: false }, { ...readiness(), ready: true }])(
    'hides when the gateway does not require a warning', value => {
      setReadiness(value);
      const { container } = renderWithProviders(<IsolationReadinessBanner />);
      expect(container).toBeEmptyDOMElement();
    });

  it('shows unfinished isolation setup even when all host checks passed', () => {
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Protected TEAM work is paused');
    expect(screen.getByRole('alert')).toHaveTextContent('Employee work, including API-only model calls, cannot start');
    expect(screen.getByText(/Document processing/)).toHaveTextContent('Setup is not complete');
    expect(screen.queryByText(/Launch service/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('alert')).toBeInTheDocument();
  });

  it('distinguishes missing current evidence from unfinished setup', () => {
    const value = readiness();
    value.components.network = 'unavailable';
    setReadiness(value);
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText(/Outbound network protection/)).toHaveTextContent('Current verification unavailable');
    expect(screen.getByText(/Tool authorization/)).toHaveTextContent('Setup is not complete');
  });

  it('explains the employee work block to non-admins without component details', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'employee' } });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Protected TEAM work is paused');
    expect(screen.getByRole('alert')).toHaveTextContent('Employee work, including API-only model calls, cannot start');
    expect(screen.queryByText('Service details')).not.toBeInTheDocument();
  });

  it('explains missing bindings and connector setup using provider family names', () => {
    setReadiness({ ...readiness(), reasons: [
      { code: 'binding_evidence_missing' },
      { code: 'connector_setup_required', provider: 'google' },
    ] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('No protected external connection requests have been sent');
    expect(screen.getByText('Account binding setup is not complete.')).toBeInTheDocument();
    expect(screen.getByText('Google connection setup is not complete.')).toBeInTheDocument();
  });

  it.each([
    ['notion', 'Notion'], ['github', 'GitHub'], ['odoo', 'Odoo'],
    ['database', 'Database'], ['channel', 'Messaging channel'], ['search', 'Search'],
    ['model', 'Model'], ['computer_login', 'Computer login'],
  ])('names the %s provider family', (provider, label) => {
    setReadiness({ ...readiness(), reasons: [{ code: 'connector_setup_required', provider }] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText(`${label} connection setup is not complete.`)).toBeInTheDocument();
  });

  it.each(['revoked', 'personal_binding_conflict'] as const)('explains revoked access: %s', code => {
    setReadiness({ ...readiness(), reasons: [{ code }] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent(code === 'revoked'
      ? 'Access has been revoked'
      : 'This AI employee’s dedicated account binding cannot currently be used in protected mode');
  });

  it('explains temporary service unavailability', () => {
    setReadiness({ ...readiness(), reasons: [{ code: 'service_unavailable' }] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText('The service is temporarily unavailable. Check again later.')).toBeInTheDocument();
  });

  it('uses the Personal account notice on Personal installations', () => {
    setReadiness({ ...readiness(), reasons: [] });
    useSystemStore.setState({ status: { ...useSystemStore.getState().status!, edition_profile: 'personal' } });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('This AI employee’s dedicated account binding cannot currently be used in protected mode');
  });

  it('shows administrators the support reference and existing security settings route', () => {
    setReadiness({ ...readiness(), reasons: [{ code: 'service_unavailable' }], correlation_id: 'support-ref-123' });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText('Reference number: support-ref-123')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'View isolation status and settings' }))
      .toHaveAttribute('href', '/app/system/security');
  });

  it.each(['employee', 'manager', undefined] as const)('hides internal details and support references from non-admins: %s', role => {
    useAuthStore.setState({ user: role ? { ...useAuthStore.getState().user!, role } : null });
    setReadiness({ ...readiness(), reasons: [
      { code: 'connector_setup_required', provider: 'google' },
      { code: 'connector_setup_required', provider: 'internal-connector-42' },
    ], correlation_id: 'private-reference' });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText('Google connection setup is not complete.')).toBeInTheDocument();
    expect(screen.getByText('External connection service setup is not complete.')).toBeInTheDocument();
    expect(screen.getByRole('alert')).not.toHaveTextContent('internal-connector-42');
    expect(screen.getByRole('alert')).not.toHaveTextContent('private-reference');
    expect(screen.queryByText(/Reference number/)).not.toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.queryByText('Service details')).not.toBeInTheDocument();
  });

  it('preserves the legacy description when reasons are absent', () => {
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Required isolation services are not ready.');
    expect(screen.queryByText(/No protected external connection requests/)).not.toBeInTheDocument();
  });

});
