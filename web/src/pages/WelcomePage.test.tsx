import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useTourStore } from '@/stores/tour-store';

// The wizard probes runtime + template availability on mount; a "no templates"
// payload keeps it to the plain 4-step flow (industry step auto-skips later).
vi.mock('@/lib/api', () => ({
  api: {
    runtime: { detect: vi.fn().mockResolvedValue({}) },
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
    accounts: { add: vi.fn() },
    inference: { update: vi.fn() },
    system: { updateConfig: vi.fn() },
  },
}));

import { WelcomePage } from './WelcomePage';

/**
 * WP5.1 — WelcomePage Multica migration smoke test. Locks in the §5.8
 * two-column hero/side-panel step-1 landing.
 */
beforeEach(() => {
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
