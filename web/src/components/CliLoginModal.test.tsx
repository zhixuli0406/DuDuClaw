import { describe, it, expect, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';

vi.mock('@/lib/api', () => ({
  api: { auth: { cliLoginStart: vi.fn(), cliLoginStatus: vi.fn(), cliLoginCancel: vi.fn(), cliLoginInput: vi.fn() } },
}));
vi.mock('@/lib/ws-client', () => ({
  client: { subscribe: () => () => {}, call: vi.fn() },
}));

import { CliLoginModal, type LoginRuntime } from './CliLoginModal';
import { api } from '@/lib/api';

const ANTHROPIC = 'https://code.claude.com/docs/en/legal-and-compliance';

/** Each runtime's disclosure shows that runtime's vendor terms (review of
 *  PR #135: Codex, Gemini and Grok used to see only Anthropic's text). */
describe('<CliLoginModal> risk disclosure', () => {
  it.each([
    ['claude', ANTHROPIC],
    ['codex', 'https://openai.com/policies/sign-in-with-chatgpt-terms/'],
    ['grok', 'https://x.ai/legal/terms-of-service'],
  ] as const)('%s links %s', (runtime, href) => {
    renderWithProviders(<CliLoginModal open runtime={runtime as LoginRuntime} onClose={vi.fn()} />);
    const disclosure = screen.getByTestId('subscription-risk-disclosure');
    const links = Array.from(disclosure.querySelectorAll('a')).map((a) => a.getAttribute('href'));
    expect(links).toEqual([href]);
    if (runtime !== 'claude') {
      expect(disclosure.textContent).not.toMatch(/Anthropic/);
    }
  });
});

describe('<CliLoginModal> session lifetime', () => {
  it('cancels a login that starts after the dialog closes', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    let resolveStart!: (value: Awaited<ReturnType<typeof api.auth.cliLoginStart>>) => void;
    vi.mocked(api.auth.cliLoginStart).mockImplementationOnce(() => new Promise((resolve) => {
      resolveStart = resolve;
    }));
    vi.mocked(api.auth.cliLoginCancel).mockResolvedValueOnce({ success: true });

    const view = renderWithProviders(<CliLoginModal open runtime="codex" onClose={onClose} />);
    await user.click(screen.getByRole('checkbox', { name: /read the policy and risks/i }));
    await user.click(screen.getByRole('button', { name: /^continue$/i }));
    await waitFor(() => expect(api.auth.cliLoginStart).toHaveBeenCalledWith('codex', true));
    await user.click(screen.getByRole('button', { name: /^cancel$/i }));
    expect(onClose).toHaveBeenCalledOnce();
    view.rerender(<CliLoginModal open={false} runtime="codex" onClose={onClose} />);

    resolveStart({
      session_id: 'late-login', runtime: 'codex', program: 'codex',
      remote_safe: true, hint: '', status: 'running',
    });
    await waitFor(() => expect(api.auth.cliLoginCancel).toHaveBeenCalledWith('late-login'));
    expect(api.auth.cliLoginStatus).not.toHaveBeenCalled();
  });
});
