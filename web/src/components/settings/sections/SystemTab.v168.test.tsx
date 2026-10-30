import { describe, it, expect, beforeEach, vi } from 'vitest';
import userEvent from '@testing-library/user-event';
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
  it('updates the blank model placeholder when the global provider changes', async () => {
    const user = userEvent.setup();
    await ready();
    const model = screen.getByRole('textbox', { name: 'Global utility model' });
    expect(model).toHaveAttribute('placeholder', 'claude-haiku-4-5');
    await user.click(screen.getByRole('combobox', { name: 'Global utility provider' }));
    await user.click(await screen.findByRole('option', { name: 'Codex' }));
    expect(model).toHaveAttribute('placeholder', "Use the backend's default");
    expect(model).toHaveValue('');
  });

  it('warns for a known mismatched global utility model and clears the warning on correction', async () => {
    configMock.mockResolvedValue({ config: BASE + '\n[runtime]\nutility_provider = "codex"\n' });
    await ready();
    const model = screen.getByRole('textbox', { name: 'Global utility model' });
    const warning = /The model belongs to a different vendor family/;
    for (const value of ['claude-haiku-4-5', 'gemini-2.5-flash', 'grok-4']) {
      fireEvent.change(model, { target: { value } });
      expect(screen.getByRole('alert')).toHaveTextContent(warning);
    }
    for (const value of ['gpt-5.4', 'custom-model', '']) {
      fireEvent.change(model, { target: { value } });
      expect(screen.queryByText(warning)).not.toBeInTheDocument();
    }
  });

  it('loads global utility settings and saves only edited keys', async () => {
    configMock.mockResolvedValue({ config: BASE + '\n[runtime]\nutility_provider = "codex"\nutility_model = "gpt-5.4"\n' });
    await ready();
    expect(screen.getByRole('combobox', { name: 'Global utility provider' })).toHaveTextContent('Codex');
    const model = screen.getByRole('textbox', { name: 'Global utility model' });
    expect(model).toHaveValue('gpt-5.4');
    fireEvent.change(model, { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ runtime: { utility_model: '' } }));
  });

  it.each([['gemini', 'Gemini'], ['antigravity', 'Antigravity'], ['agy', 'Antigravity']])('keeps the removed global %s provider visible and allows replacement with Claude', async (provider, name) => {
    const user = userEvent.setup();
    configMock.mockResolvedValue({ config: BASE + `\n[runtime]\nutility_provider = "${provider}"\n`, utility_provider: provider });
    await ready();
    expect(screen.getByRole('alert')).toHaveTextContent('The saved provider has been removed');
    expect(screen.getByRole('combobox', { name: 'Global utility provider' })).toHaveTextContent(name);
    await user.click(screen.getByRole('combobox', { name: 'Global utility provider' }));
    expect(await screen.findByRole('option', { name })).toBeInTheDocument();
    await user.click(await screen.findByRole('option', { name: 'Claude' }));
    await user.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ runtime: { utility_provider: 'claude' } }));
  });

  it('uses the effective global provider returned by the server for legacy values', async () => {
    configMock.mockResolvedValue({ config: BASE + '\n[runtime]\nutility_provider = "legacy-unknown"\n', utility_provider: 'claude' });
    useAgentsStore.setState({ loaded: true, error: null, agents: [{ name: 'coder', runtime: { provider: 'codex' } }] as never });
    await ready();
    expect(screen.getByRole('alert')).toHaveTextContent('Global background tasks still use Claude');
  });

  it('warns for default Claude global utility with no Claude employees', async () => {
    useAgentsStore.setState({ loaded: true, error: null, agents: [{ name: 'coder', runtime: { provider: 'codex' } }] as never });
    await ready();
    expect(screen.getByRole('alert')).toHaveTextContent('Global background tasks still use Claude');
  });

  it('does not warn for a non-Claude global utility or a Claude employee', async () => {
    configMock.mockResolvedValue({ config: BASE + '\n[runtime]\nutility_provider = "codex"\n' });
    useAgentsStore.setState({ loaded: true, error: null, agents: [] });
    await ready();
    expect(screen.queryByText(/Global background tasks still use Claude/)).not.toBeInTheDocument();
  });

  it('does not warn when an employee uses the default Claude runtime', async () => {
    useAgentsStore.setState({ loaded: true, error: null, agents: [{ name: 'claude-worker' }] as never });
    await ready();
    expect(screen.queryByText(/Global background tasks still use Claude/)).not.toBeInTheDocument();
  });

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
