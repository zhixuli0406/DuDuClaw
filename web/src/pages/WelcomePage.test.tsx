import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useTourStore } from '@/stores/tour-store';

// The wizard probes runtime + template availability on mount; a "no templates"
// payload keeps it to the plain 4-step flow (industry step auto-skips later).
vi.mock('@/lib/api', () => ({
  api: {
    runtime: { detect: vi.fn().mockResolvedValue({}), install: vi.fn() },
    templates: {
      industries: vi.fn().mockResolvedValue({
        unlocked: false,
        present_but_locked: false,
        staged: null,
        ceo_available: false,
        industries: [],
      }),
      roster: vi.fn().mockResolvedValue({ roles: [] }),
      role: vi.fn(),
      stage: vi.fn(),
      createAgent: vi.fn(),
    },
    agents: { create: vi.fn(), update: vi.fn() },
    accounts: { add: vi.fn(), cliCredentials: vi.fn().mockResolvedValue({ credentials: [] }) },
    auth: {
      cliLoginStart: vi.fn(),
      cliLoginInput: vi.fn(),
      cliLoginStatus: vi.fn(),
      cliLoginCancel: vi.fn(),
      cliLoginFinalize: vi.fn(),
    },
    inference: { update: vi.fn() },
    system: { updateConfig: vi.fn() },
  },
}));

import { api, type RuntimeDetect } from '@/lib/api';
import { WelcomePage } from './WelcomePage';

/**
 * WP5.1 — WelcomePage Multica migration smoke test. Locks in the §5.8
 * two-column hero/side-panel step-1 landing.
 *
 * WP2 / D16 — the backend step's setup flow. Regressions locked down here, all
 * reported from the 2026-08-04 live run:
 *  - detection was probed once per mount, so re-detecting kept returning the
 *    stale first answer and the only real fix was closing the app and reopening.
 *  - a missing CLI was a dead "未安裝" label with nothing to press.
 *  - a CLI that was installed but signed out was never asked to sign in.
 */

const DETECT_ABSENT: RuntimeDetect = {
  claude_cli: false,
  codex: false,
  gemini: false,
  antigravity: false,
  grok: false,
  claude_oauth: false,
  claude_subscription: null,
};

function mockDetect(overrides: Partial<typeof DETECT_ABSENT> = {}) {
  const fn = vi.mocked(api.runtime.detect);
  fn.mockResolvedValue({ ...DETECT_ABSENT, ...overrides });
  return fn;
}

/** Advance the wizard from the intro card to the backend step. */
async function gotoBackendStep(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'Get started' }));
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.subscribe.mockReturnValue(vi.fn());
  vi.mocked(api.templates.industries).mockResolvedValue({
    unlocked: false,
    present_but_locked: false,
    staged: null,
    ceo_available: false,
    industries: [],
  });
  vi.mocked(api.templates.roster).mockResolvedValue({
    industry: '',
    label: '',
    roles: [],
    humans: [],
    excluded: [],
  });
  vi.mocked(api.accounts.cliCredentials).mockResolvedValue({ credentials: [] });
  mockDetect();
  useAgentsStore.setState({ fetchAgents: vi.fn() as never });
  useTourStore.setState({ requestPrompt: vi.fn() as never });
  try {
    sessionStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('<WelcomePage>', () => {
  it('renders the step-1 hero and the get-started CTA', async () => {
    renderWithProviders(<WelcomePage />);

    expect(await screen.findByRole('heading', { name: /create your first agent/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Get started' })).toBeInTheDocument();
  });
});

describe('<WelcomePage> runtime setup (WP2 / D16)', () => {
  it('re-probes when the re-detect button is pressed (no app restart needed)', async () => {
    const user = userEvent.setup();
    const detect = mockDetect();
    renderWithProviders(<WelcomePage />);
    await waitFor(() => expect(detect).toHaveBeenCalled());

    await gotoBackendStep(user);
    expect(await screen.findByText(/choose an ai service/i)).toBeInTheDocument();
    const before = detect.mock.calls.length;

    await user.click(screen.getAllByRole('button', { name: /check again/i })[0]);
    await waitFor(() => expect(detect.mock.calls.length).toBeGreaterThan(before));
  });

  it('offers an install button for a backend that is not installed', async () => {
    const user = userEvent.setup();
    mockDetect();
    renderWithProviders(<WelcomePage />);
    await gotoBackendStep(user);

    await user.click(await screen.findByRole('button', { name: /i have a claude subscription/i }));
    expect(await screen.findByRole('button', { name: /install it for me/i })).toBeInTheDocument();
  });

  it('asks an installed-but-signed-out CLI to sign in, in place', async () => {
    const user = userEvent.setup();
    mockDetect({ claude_cli: true, claude_oauth: false });
    renderWithProviders(<WelcomePage />);
    await gotoBackendStep(user);

    await user.click(await screen.findByRole('button', { name: /i have a claude subscription/i }));
    expect(await screen.findByRole('button', { name: /sign in now/i })).toBeInTheDocument();
    // Not the old dead "run claude in a terminal" hint.
    expect(screen.queryByRole('button', { name: /install it for me/i })).not.toBeInTheDocument();
  });

  it('confirms the plan and asks for nothing when already signed in', async () => {
    const user = userEvent.setup();
    mockDetect({ claude_cli: true, claude_oauth: true, claude_subscription: 'Max' });
    renderWithProviders(<WelcomePage />);
    await gotoBackendStep(user);

    await user.click(await screen.findByRole('button', { name: /i have a claude subscription/i }));
    expect(await screen.findByText(/signed in to claude \(plan: Max\)/i)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /install it for me/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /sign in now/i })).not.toBeInTheDocument();
  });

  it('surfaces sign-in for the picked "other CLI" too', async () => {
    const user = userEvent.setup();
    mockDetect({ gemini: true });
    vi.mocked(api.accounts.cliCredentials).mockResolvedValue({
      credentials: [
        {
          runtime: 'gemini',
          store: '~/.gemini/oauth_creds.json',
          installed: true,
          present: false,
          modified_at: null,
        },
      ],
    });
    renderWithProviders(<WelcomePage />);
    await gotoBackendStep(user);

    // 'gemini' is the wizard's default pick among the other CLIs.
    await user.click(await screen.findByRole('button', { name: /i use a different ai tool/i }));
    expect(await screen.findByRole('button', { name: /sign in now/i })).toBeInTheDocument();
  });

  it('degrades quietly when detection fails (no crash, no false prompts)', async () => {
    const user = userEvent.setup();
    vi.mocked(api.runtime.detect).mockRejectedValue(new Error('gateway down'));
    renderWithProviders(<WelcomePage />);
    await gotoBackendStep(user);

    expect(await screen.findByText(/choose an ai service/i)).toBeInTheDocument();
    await user.click(await screen.findByRole('button', { name: /i have a claude subscription/i }));
    // No detect result ⇒ no install / sign-in prompts at all.
    expect(screen.queryByRole('button', { name: /install it for me/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /sign in now/i })).not.toBeInTheDocument();
  });
});
