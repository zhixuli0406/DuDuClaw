import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RuntimeSetupCard } from './RuntimeSetupCard';
import { api } from '@/lib/api';

/**
 * WP2 / D16 state flow: 未安裝 → 安裝中 → 已安裝, and 已安裝未登入 → 登入.
 *
 * These are the exact transitions the onboarding wizard's step 2 walks a
 * first-time user through, and the reason the card exists: before it, both
 * "not installed" and "not signed in" were dead labels.
 */

/** Grab the handler the component registered for a gateway event. */
function eventHandler(event: string): (payload: unknown) => void {
  const call = mockWsClient.subscribe.mock.calls.find((c) => c[0] === event);
  if (!call) throw new Error(`no subscriber for ${event}`);
  return call[1] as (payload: unknown) => void;
}

beforeEach(() => {
  vi.restoreAllMocks();
  mockWsClient.subscribe.mockClear();
  mockWsClient.subscribe.mockReturnValue(vi.fn());
});

describe('RuntimeSetupCard — install flow', () => {
  it('renders nothing until the first detect answers (no wrong state flash)', () => {
    const { container } = renderWithProviders(
      <RuntimeSetupCard provider="claude" installed={undefined} onDetect={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('walks 未安裝 → 安裝中 → 已安裝 and re-detects on success', async () => {
    const user = userEvent.setup();
    const onDetect = vi.fn();
    const install = vi.spyOn(api.runtime, 'install').mockResolvedValue({
      started: true,
      provider: 'claude',
      session_id: 'sess-1',
      command: 'npm install -g @anthropic-ai/claude-code',
      docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
      timeout_secs: 300,
    });

    renderWithProviders(
      <RuntimeSetupCard provider="claude" installed={false} onDetect={onDetect} />,
    );

    // 未安裝 — an actionable button, not a dead label.
    const btn = screen.getByRole('button', { name: /install it for me/i });
    await user.click(btn);

    // The provider name is the ONLY thing sent; the command lives server-side.
    await waitFor(() => expect(install).toHaveBeenCalledWith('claude'));

    // 安裝中 — a live transcript appears.
    expect(await screen.findByTestId('runtime-install-output')).toBeInTheDocument();

    // Streamed output lands in the transcript.
    eventHandler('runtime.install.output')({
      session_id: 'sess-1',
      provider: 'claude',
      data: 'added 1 package\n',
    });
    await waitFor(() =>
      expect(screen.getByTestId('runtime-install-output')).toHaveTextContent('added 1 package'),
    );

    // 已安裝 — terminal status with a confirmed binary triggers a re-detect.
    eventHandler('runtime.install.status')({
      session_id: 'sess-1',
      provider: 'claude',
      status: 'succeeded',
      exit_code: 0,
      detected: true,
      command: 'npm install -g @anthropic-ai/claude-code',
      docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
    });
    await waitFor(() => expect(onDetect).toHaveBeenCalled());
  });

  it('treats a zero exit that produced no binary as a failure, not success', async () => {
    const user = userEvent.setup();
    const onDetect = vi.fn();
    vi.spyOn(api.runtime, 'install').mockResolvedValue({
      started: true,
      provider: 'gemini',
      session_id: 'sess-2',
      command: 'npm install -g @google/gemini-cli',
      docs_url: 'https://github.com/google-gemini/gemini-cli',
      timeout_secs: 300,
    });

    renderWithProviders(
      <RuntimeSetupCard provider="gemini" installed={false} onDetect={onDetect} />,
    );
    await user.click(screen.getByRole('button', { name: /install it for me/i }));
    await waitFor(() => expect(screen.getByTestId('runtime-install-output')).toBeInTheDocument());

    eventHandler('runtime.install.status')({
      session_id: 'sess-2',
      provider: 'gemini',
      status: 'succeeded',
      exit_code: 0,
      detected: false,
      command: 'npm install -g @google/gemini-cli',
      docs_url: 'https://github.com/google-gemini/gemini-cli',
    });

    // Falls back to the copyable command rather than turning the wizard green.
    expect(await screen.findByText('npm install -g @google/gemini-cli')).toBeInTheDocument();
    expect(onDetect).not.toHaveBeenCalled();
  });

  it('ignores events from another install session', async () => {
    const user = userEvent.setup();
    const onDetect = vi.fn();
    vi.spyOn(api.runtime, 'install').mockResolvedValue({
      started: true,
      provider: 'codex',
      session_id: 'mine',
      command: 'npm install -g @openai/codex',
      docs_url: 'https://github.com/openai/codex',
      timeout_secs: 300,
    });
    renderWithProviders(
      <RuntimeSetupCard provider="codex" installed={false} onDetect={onDetect} />,
    );
    await user.click(screen.getByRole('button', { name: /install it for me/i }));
    await waitFor(() => expect(screen.getByTestId('runtime-install-output')).toBeInTheDocument());

    eventHandler('runtime.install.status')({
      session_id: 'someone-else',
      provider: 'codex',
      status: 'succeeded',
      exit_code: 0,
      detected: true,
      command: 'npm install -g @openai/codex',
      docs_url: 'https://github.com/openai/codex',
    });
    expect(onDetect).not.toHaveBeenCalled();
  });

  it('shows the copyable command when the gateway declines to install', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.runtime, 'install').mockResolvedValue({
      started: false,
      provider: 'claude',
      reason: 'prerequisite_missing',
      command: 'npm install -g @anthropic-ai/claude-code',
      prerequisite: 'npm',
      docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
    });

    renderWithProviders(<RuntimeSetupCard provider="claude" installed={false} onDetect={vi.fn()} />);
    await user.click(screen.getByRole('button', { name: /install it for me/i }));

    // The missing prerequisite is named, and the command is still offered.
    expect(await screen.findByText(/npm isn't on this computer/i)).toBeInTheDocument();
    expect(
      screen.getByText('npm install -g @anthropic-ai/claude-code'),
    ).toBeInTheDocument();
  });

  it('admits it does not know the outcome when no terminal event ever arrives', async () => {
    // A dropped socket / restarted gateway mid-install used to leave the card
    // spinning forever with the button disabled. The local watchdog (armed from
    // the gateway's own timeout_secs) must hand control back instead.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      vi.spyOn(api.runtime, 'install').mockResolvedValue({
        started: true,
        provider: 'claude',
        session_id: 'sess-lost',
        command: 'npm install -g @anthropic-ai/claude-code',
        docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
        timeout_secs: 300,
      });

      renderWithProviders(
        <RuntimeSetupCard provider="claude" installed={false} onDetect={vi.fn()} />,
      );
      await user.click(screen.getByRole('button', { name: /install it for me/i }));
      await waitFor(() => expect(screen.getByTestId('runtime-install-output')).toBeInTheDocument());
      expect(screen.getByRole('button', { name: /installing/i })).toBeDisabled();

      // Past the server's own deadline plus the grace window.
      await vi.advanceTimersByTimeAsync((300 + 21) * 1000);

      expect(await screen.findByText(/install status unknown/i)).toBeInTheDocument();
      // Button is usable again — not stuck on "Installing…".
      expect(screen.getByRole('button', { name: /install it for me/i })).toBeEnabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not fire the watchdog once a terminal status arrives', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const onDetect = vi.fn();
      vi.spyOn(api.runtime, 'install').mockResolvedValue({
        started: true,
        provider: 'claude',
        session_id: 'sess-ok',
        command: 'npm install -g @anthropic-ai/claude-code',
        docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
        timeout_secs: 300,
      });
      renderWithProviders(
        <RuntimeSetupCard provider="claude" installed={false} onDetect={onDetect} />,
      );
      await user.click(screen.getByRole('button', { name: /install it for me/i }));
      await waitFor(() => expect(screen.getByTestId('runtime-install-output')).toBeInTheDocument());

      eventHandler('runtime.install.status')({
        session_id: 'sess-ok',
        provider: 'claude',
        status: 'succeeded',
        exit_code: 0,
        detected: true,
        command: 'npm install -g @anthropic-ai/claude-code',
        docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
      });
      await waitFor(() => expect(onDetect).toHaveBeenCalled());

      await vi.advanceTimersByTimeAsync((300 + 60) * 1000);
      expect(screen.queryByText(/install status unknown/i)).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('re-detects instead of reinstalling when the CLI turns out to be present', async () => {
    const user = userEvent.setup();
    const onDetect = vi.fn();
    vi.spyOn(api.runtime, 'install').mockResolvedValue({
      started: false,
      provider: 'claude',
      reason: 'already_installed',
      command: 'npm install -g @anthropic-ai/claude-code',
      prerequisite: null,
      docs_url: 'https://docs.anthropic.com/en/docs/claude-code',
    });
    renderWithProviders(
      <RuntimeSetupCard provider="claude" installed={false} onDetect={onDetect} />,
    );
    await user.click(screen.getByRole('button', { name: /install it for me/i }));
    await waitFor(() => expect(onDetect).toHaveBeenCalled());
  });
});

describe('RuntimeSetupCard — login + re-detect', () => {
  it('offers an in-place sign-in when installed but not signed in', async () => {
    renderWithProviders(
      <RuntimeSetupCard provider="claude" installed loggedIn={false} onDetect={vi.fn()} />,
    );
    expect(screen.getByText(/installed but not signed in/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /sign in now/i })).toBeInTheDocument();
    // Not a reinstall prompt.
    expect(screen.queryByRole('button', { name: /install it for me/i })).not.toBeInTheDocument();
  });

  it('opens the PTY login modal from the sign-in button', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.auth, 'cliLoginStart').mockResolvedValue({
      session_id: 'login-1',
      runtime: 'claude',
      program: '/usr/local/bin/claude',
      remote_safe: true,
      hint: 'paste the code below',
      status: 'running',
    });
    renderWithProviders(
      <RuntimeSetupCard provider="claude" installed loggedIn={false} onDetect={vi.fn()} />,
    );
    await user.click(screen.getByRole('button', { name: /sign in now/i }));
    await waitFor(() => expect(api.auth.cliLoginStart).toHaveBeenCalledWith('claude'));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
  });

  it('reports ready once installed and signed in', () => {
    renderWithProviders(
      <RuntimeSetupCard provider="codex" installed loggedIn onDetect={vi.fn()} />,
    );
    expect(screen.getByText(/is ready/i)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /install it for me/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /sign in now/i })).not.toBeInTheDocument();
  });

  it('hides the sign-in row when the provider has no observable login state', () => {
    // Antigravity keeps no credential file we can probe — `loggedIn` is
    // undefined and must NOT be read as "not signed in".
    renderWithProviders(
      <RuntimeSetupCard provider="antigravity" installed onDetect={vi.fn()} />,
    );
    expect(screen.queryByRole('button', { name: /sign in now/i })).not.toBeInTheDocument();
    expect(screen.getByText(/is ready/i)).toBeInTheDocument();
  });

  it('re-probes on demand without a page reload (the "要關掉重來" bug)', async () => {
    const user = userEvent.setup();
    const onDetect = vi.fn();
    renderWithProviders(
      <RuntimeSetupCard provider="gemini" installed={false} onDetect={onDetect} />,
    );
    await user.click(screen.getByRole('button', { name: /check again/i }));
    expect(onDetect).toHaveBeenCalledTimes(1);
  });

  it('always offers a self-install escape hatch when not installed', () => {
    renderWithProviders(
      <RuntimeSetupCard provider="antigravity" installed={false} onDetect={vi.fn()} />,
    );
    const link = screen.getByRole('link', { name: /install it myself/i });
    expect(link).toHaveAttribute('href', 'https://antigravity.google/docs/cli');
  });
});
