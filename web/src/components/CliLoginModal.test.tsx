import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

vi.mock('@/lib/api', () => ({
  api: { auth: { cliLoginStart: vi.fn(), cliLoginStatus: vi.fn(), cliLoginCancel: vi.fn(), cliLoginInput: vi.fn() } },
}));
vi.mock('@/lib/ws-client', () => ({
  client: { subscribe: () => () => {}, call: vi.fn() },
}));

import { CliLoginModal, type LoginRuntime } from './CliLoginModal';

const ANTHROPIC = 'https://code.claude.com/docs/en/legal-and-compliance';

/** Each runtime's disclosure shows that runtime's vendor terms (review of
 *  PR #135: Codex, Gemini and Grok used to see only Anthropic's text). */
describe('<CliLoginModal> risk disclosure', () => {
  it.each([
    ['claude', ANTHROPIC],
    ['codex', 'https://openai.com/policies/sign-in-with-chatgpt-terms/'],
    ['antigravity', 'https://antigravity.google/terms'],
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
