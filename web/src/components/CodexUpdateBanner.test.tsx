import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { api, type RuntimeDetect } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { CodexUpdateBanner } from './CodexUpdateBanner';

const detection = (required = true): RuntimeDetect => ({
  claude_cli: false, codex: true, claude_oauth: false, claude_subscription: null,
  runtimes: [{ id: 'codex', version: '0.163.0', minimum_version: '0.164.0', update_required: required }],
});

function status(payload: Record<string, unknown>) {
  const handlers = mockWsClient.subscribe.mock.calls.filter((c) => c[0] === 'runtime.install.status');
  act(() => {
    for (const call of handlers) (call[1] as (p: unknown) => void)({
      session_id: 'update-1', provider: 'codex', detected: true, exit_code: 0,
      command: 'npm install -g @openai/codex', docs_url: 'https://github.com/openai/codex',
      ...payload,
    });
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  mockWsClient.subscribe.mockClear();
  mockWsClient.subscribe.mockReturnValue(vi.fn());
  useAuthStore.setState({ user: {
    id: 'admin', email: 'admin@test.invalid', display_name: 'Admin', role: 'admin', status: 'active',
  } });
  vi.spyOn(api.runtime, 'detect').mockResolvedValue(detection());
  vi.spyOn(api.runtime, 'install').mockResolvedValue({
    started: true, provider: 'codex', session_id: 'update-1',
    command: 'npm install -g @openai/codex', docs_url: 'https://github.com/openai/codex', timeout_secs: 300,
  });
});

afterEach(() => { useAuthStore.setState({ user: null }); });

describe('CodexUpdateBanner', () => {
  it('is hidden for non-admins without probing', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'manager' } });
    const { container } = renderWithProviders(<CodexUpdateBanner />);
    expect(container).toBeEmptyDOMElement();
    expect(api.runtime.detect).not.toHaveBeenCalled();
  });

  it('is hidden when no update is required', async () => {
    vi.mocked(api.runtime.detect).mockResolvedValue(detection(false));
    const { container } = renderWithProviders(<CodexUpdateBanner />);
    await waitFor(() => expect(api.runtime.detect).toHaveBeenCalled());
    expect(container).toBeEmptyDOMElement();
  });

  it('updates codex and refreshes detection on success', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CodexUpdateBanner />);
    await user.click(await screen.findByRole('button', { name: 'Update Codex CLI' }));
    expect(api.runtime.install).toHaveBeenCalledWith('codex');
    vi.mocked(api.runtime.detect).mockResolvedValue(detection(false));
    status({ status: 'succeeded' });
    await waitFor(() => expect(api.runtime.detect).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText(/Codex CLI needs/)).not.toBeInTheDocument());
  });

  it('shows the shadowing path and copyable manual command', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CodexUpdateBanner />);
    await user.click(await screen.findByRole('button', { name: 'Update Codex CLI' }));
    status({ status: 'update_shadowed', reason: 'update_shadowed', path: '/opt/homebrew/bin/codex' });
    expect(await screen.findByText(/older Codex CLI.*\/opt\/homebrew\/bin\/codex/)).toBeInTheDocument();
    expect(screen.getByText('npm install -g @openai/codex')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /copy/i })).toBeInTheDocument();
    expect(api.runtime.detect).toHaveBeenCalledTimes(1);
  });

  it('shows a fallback command on failure', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CodexUpdateBanner />);
    await user.click(await screen.findByRole('button', { name: 'Update Codex CLI' }));
    status({ status: 'failed', detected: false, exit_code: 1 });
    expect(await screen.findByText('npm install -g @openai/codex')).toBeInTheDocument();
  });
});
