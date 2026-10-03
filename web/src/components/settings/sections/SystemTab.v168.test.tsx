import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

// v1.68 (W2) additions to 設定 → 系統: sandbox / computer-use image / trust
// guard / file roots / OTLP / A2A trust / GitHub / secret-backend fields, the
// changed-keys-only payload and the restart banner feed.
const configMock = vi.fn();
const updateConfigMock = vi.fn();
const statusMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
      status: () => statusMock(),
    },
  },
}));

import { SystemTab } from './SystemTab';

const BASE = `[gateway]
bind = "127.0.0.1"
port = 3100

[secret_manager]
backend = "onepassword"
onepassword_host = "https://op.internal"
onepassword_token_enc = "********"

[container.sandbox]
memory_bytes = 1073741824
tmp_bytes = 268435456
workspace_bytes = 536870912

[integrations]
github = true
`;

function setAdmin(admin: boolean) {
  useAuthStore.setState({ user: admin ? ({ role: 'admin' } as never) : ({ role: 'employee' } as never) });
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useRestartRequiredStore.getState().clear();
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
  configMock.mockResolvedValue({ config: BASE, allowed_origins: [] });
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
  statusMock.mockResolvedValue({ otel_compiled: false });
  setAdmin(true);
});

async function ready() {
  renderWithProviders(<SystemTab />);
  await waitFor(() => expect(screen.getByRole('switch', { name: 'GitHub tools' })).toBeChecked());
}

describe('<SystemTab> v1.68', () => {
  it('treats a stored plain log format as pretty and does not rewrite it', async () => {
    configMock.mockResolvedValue({ config: BASE + '\n[logging]\nformat = "plain"\n', allowed_origins: [] });
    await ready();
    fireEvent.click(screen.getByRole('switch', { name: 'GitHub tools' }));
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ integrations: { github: false } });
  });

  it('sends only the keys that changed', async () => {
    await ready();
    fireEvent.click(screen.getByRole('switch', { name: 'GitHub tools' }));
    fireEvent.click(screen.getByRole('switch', { name: 'Memory trust guard' }));
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({
      integrations: { github: false },
      memory: { supersession_trust_guard: false },
    });
  });

  it('shows the onepassword fields for that backend and sends a typed token write-only', async () => {
    localStorage.setItem('dudu.settings.advanced.settings.system.secrets', '1');
    await ready();
    expect(screen.getByDisplayValue('https://op.internal')).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText('1Password Connect access token'), { target: { value: 'tok-123' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ secret_manager: { onepassword_token: 'tok-123' } });
  });

  it('blocks a sandbox whose tmpfs sizes exceed memory', async () => {
    localStorage.setItem('dudu.settings.advanced.settings.system.sandboxLimits', '1');
    await ready();
    fireEvent.change(screen.getByLabelText('Workspace space'), { target: { value: '2048' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await new Promise((r) => setTimeout(r, 20));
    expect(updateConfigMock).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText('Workspace space'), { target: { value: '256' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ container: { sandbox: { workspace_bytes: 256 * 1024 * 1024 } } });
  });

  it('explains that the build has no tracing instead of offering the OTLP field', async () => {
    await ready();
    expect(screen.getByTestId('otel-not-compiled')).toBeInTheDocument();
  });

  it('offers the OTLP field when tracing is compiled in and feeds the restart banner', async () => {
    statusMock.mockResolvedValue({ otel_compiled: true });
    updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: ['telemetry.otlp_endpoint'] });
    await ready();
    const field = await screen.findByLabelText('Send OpenTelemetry traces to');
    fireEvent.change(field, { target: { value: 'http://collector:4317' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ telemetry: { otlp_endpoint: 'http://collector:4317' } });
    await waitFor(() => expect(useRestartRequiredStore.getState().keys).toEqual(['telemetry.otlp_endpoint']));
  });

  it('shows A2A trust to admins only, with a warning when on', async () => {
    await ready();
    fireEvent.click(screen.getByRole('switch', { name: 'Trust outside A2A requests' }));
    expect(screen.getByText(/any A2A client that can reach this gateway/)).toBeInTheDocument();
  });

  it('hides A2A trust from non-admins', async () => {
    setAdmin(false);
    await ready();
    expect(screen.queryByRole('switch', { name: 'Trust outside A2A requests' })).not.toBeInTheDocument();
  });

  it('adds an absolute local-file root and refuses a relative one', async () => {
    await ready();
    const input = screen.getByLabelText('Add a folder (full path)');
    fireEvent.change(input, { target: { value: 'exports' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add folder' }));
    expect(screen.getByText(/Use a full path/)).toBeInTheDocument();
    fireEvent.change(input, { target: { value: '/srv/exports' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add folder' }));
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ files: { allowed_roots: ['/srv/exports'] } });
  });
});
