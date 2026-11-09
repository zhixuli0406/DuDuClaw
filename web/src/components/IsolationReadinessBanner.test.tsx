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

  it.each([
    ['binding_evidence_missing', undefined, 'Account binding setup is not complete.',
      'Finish employee setup in migration confirmations', '/app/system/migration'],
    ['connector_setup_required', 'database', 'Database connection setup is not complete.',
      'Go to connector settings', '/manage/integrations'],
    ['connector_setup_required', undefined, 'External connection service setup is not complete.',
      'Go to connector settings', '/manage/integrations'],
    ['personal_binding_conflict', undefined,
      'This AI employee’s dedicated account binding cannot currently be used in protected mode. Ask an administrator to check isolation readiness.',
      'Check members in user management', '/manage/users'],
    ['revoked', undefined, 'Access has been revoked. Ask an administrator to check the account binding.',
      'Reconnect in connector settings', '/manage/integrations'],
    ['service_unavailable', undefined, 'The service is temporarily unavailable. Check again later.',
      'View service status', '/app/system/security'],
  ] as const)('renders %s (%s) with an admin next step', (code, provider, message, hint, href) => {
    setReadiness({ ...readiness(), status: code === 'service_unavailable' ? 'service_unavailable' : 'action_required',
      reasons: [provider ? { code, provider } : { code }] });
    renderWithProviders(<IsolationReadinessBanner />);
    const item = screen.getByText(message, { selector: 'li' });
    expect(item).toBeInTheDocument();
    expect(screen.getByRole('link', { name: hint })).toHaveAttribute('href', href);
  });

  it('keeps next-step links away from non-admins', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'employee' } });
    setReadiness({ ...readiness(), reasons: [{ code: 'connector_setup_required', provider: 'google' }],
      employees: [{ label: 'sales-bot', ready: false, reasons: [{ code: 'revoked' }] }] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText('Google connection setup is not complete.')).toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.getByRole('alert')).not.toHaveTextContent('sales-bot');
  });

  it('lists each employee that needs attention for admins', () => {
    setReadiness({ ...readiness(), reasons: [
      { code: 'connector_setup_required', provider: 'google' }, { code: 'personal_binding_conflict' },
    ], employees: [
      { label: 'team', ready: true, reasons: [] },
      { label: 'alice-personal', ready: false, reasons: [
        { code: 'connector_setup_required', provider: 'google' }, { code: 'personal_binding_conflict' }] },
    ] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText('AI employees that need attention')).toBeInTheDocument();
    expect(screen.getByText(/^alice-personal: Google connection setup is not complete\./)).toBeInTheDocument();
    expect(screen.queryByText(/^team:/)).not.toBeInTheDocument();
  });

  it('shows employee attention even when admission is open, and hides when nothing is left', () => {
    setReadiness({ ...readiness(), ready: true, status: 'action_required', reasons: [{ code: 'revoked' }] });
    const { unmount } = renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Some protected AI employees need attention');
    expect(screen.getByRole('alert')).not.toHaveTextContent('Protected TEAM work is paused');
    unmount();
    setReadiness({ ...readiness(), ready: true, status: 'ready', reasons: [] });
    const { container } = renderWithProviders(<IsolationReadinessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it.each([
    [{ code: 'workspace_broker_unavailable' as const },
      'The file access service is not ready yet, so AI employees can’t read files in their work folder for now.'],
    [{ code: 'helper_launcher_unavailable' as const, reason: 'spawnd_feature_missing' },
      'The program run service is not ready yet, so AI employees can’t run programs, analyze code or work on Office documents for now.'],
    [{ code: 'helper_runtime_missing' as const, runtime: 'duduclaw-program-v1' },
      'A component needed for running programs is not installed, so AI employees can’t use it for now.'],
    [{ code: 'helper_runtime_missing' as const, runtime: 'duduclaw-code-map-v1' },
      'A component needed for code analysis is not installed, so AI employees can’t use it for now.'],
    [{ code: 'helper_runtime_missing' as const, runtime: 'duduclaw-office-*-v1' },
      'A component needed for Office document work is not installed, so AI employees can’t use it for now.'],
    [{ code: 'helper_runtime_missing' as const, runtime: 'duduclaw-unknown-v9' },
      'A component some AI employee features need is not installed, so those features can’t be used for now.'],
    [{ code: 'helper_recording_unsupported' as const },
      'Browser recording isn’t supported on this computer yet, so AI employees can’t record browser sessions for now.'],
  ])('explains file and helper readiness in plain language: %o', (reason, message) => {
    setReadiness({ ...readiness(), ready: true, status: 'action_required', reasons: [reason] });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText(message, { selector: 'li' })).toBeInTheDocument();
    const alert = screen.getByRole('alert');
    for (const internal of ['spawnd', 'duduclaw-', 'broker', 'runtime', 'helper']) {
      expect(alert).not.toHaveTextContent(internal);
    }
    expect(screen.getByRole('link', { name: 'View service status' })).toHaveAttribute('href', '/app/system/security');
  });

  it('does not show informational notices on their own', () => {
    setReadiness({ ...readiness(), ready: true, status: 'ready', reasons: [],
      notices: [{ code: 'workspace_broker_unavailable' }, { code: 'helper_runtime_missing', runtime: 'duduclaw-code-map-v1' }] });
    const { container } = renderWithProviders(<IsolationReadinessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('preserves the legacy description when reasons are absent', () => {
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Required isolation services are not ready.');
    expect(screen.queryByText(/No protected external connection requests/)).not.toBeInTheDocument();
  });

});
