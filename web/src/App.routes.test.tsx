import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { App } from './App';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import type { SystemStatus } from '@/lib/api';

/**
 * Route-level redirects and the edition gate (WP-A). These are one-line route
 * declarations, which is exactly why they are worth a test: nothing else fails
 * when one of them silently stops redirecting, and three of them exist to close
 * a hole (an ungated Enterprise page, a weaker parallel approval entry, a dead
 * page pretending to be a live one).
 */

const AGENTS = [{ name: 'nova', display_name: 'Nova', status: 'active', role: 'main', sandboxed: false }];

function LocationProbe() {
  const { pathname, search } = useLocation();
  return <div data-testid="loc">{pathname + search}</div>;
}

function renderAppAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <App />
        <LocationProbe />
      </MemoryRouter>
    </IntlProvider>,
  );
}

async function expectLandsOn(from: string, to: string) {
  renderAppAt(from);
  await waitFor(() => expect(screen.getByTestId('loc')).toHaveTextContent(to));
}

function setEdition(profile: 'personal' | 'enterprise') {
  useSystemStore.setState({ status: { edition_profile: profile } as unknown as SystemStatus });
}

beforeEach(() => {
  vi.clearAllMocks();
  // The whole app shell mounts here, so a few background widgets fire their own
  // RPCs. A benign shape covers the list-shaped ones; `growth.snapshot` gets an
  // explicit empty answer because its store dereferences the payload.
  mockWsClient.call.mockImplementation((method: string) =>
    method === 'growth.snapshot'
      ? Promise.resolve(null)
      : Promise.resolve({ agents: AGENTS, tasks: [], events: [] }),
  );
  useAuthStore.setState({
    initialized: true,
    isAuthenticated: true,
    user: { id: 'u1', username: 'owner', display_name: 'Owner', role: 'admin' },
  } as never);
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: AGENTS as never[], loaded: true, loading: false } as never);
  setEdition('enterprise');
});

describe('App routes — legacy paths land somewhere real (WP-A §2-8/§2-13)', () => {
  it('/approvals → /inbox (one door for decisions, the better-informed one)', async () => {
    await expectLandsOn('/approvals', '/inbox');
  });

  it('/mcp → the 整合 tab that actually renders it', async () => {
    await expectLandsOn('/mcp', '/manage/integrations?tab=mcp');
  });

  it('/mcp-keys → the same tab (access keys live under 工具伺服器)', async () => {
    await expectLandsOn('/mcp-keys', '/manage/integrations?tab=mcp');
  });

  it('/odoo → the Odoo tab', async () => {
    await expectLandsOn('/odoo', '/manage/integrations?tab=odoo');
  });

  it('/wizard → /welcome (the one real first-run surface)', async () => {
    await expectLandsOn('/wizard', '/welcome');
  });

  it('/legacy-dashboard → / (the page it was an early draft of)', async () => {
    await expectLandsOn('/legacy-dashboard', '/');
  });
});

describe('App routes — Enterprise-only pages are closed on Personal (WP-A D8/D10-B)', () => {
  it('blocks the canonical /manage/distributors on Personal', async () => {
    setEdition('personal');
    await expectLandsOn('/manage/distributors', '/');
  });

  it('blocks the legacy /users alias on Personal — the alias was the hole', async () => {
    setEdition('personal');
    await expectLandsOn('/users', '/');
  });

  it('blocks the legacy /governance alias on Personal', async () => {
    setEdition('personal');
    await expectLandsOn('/governance', '/');
  });

  it('leaves the same routes reachable on Enterprise', async () => {
    renderAppAt('/manage/distributors');
    await waitFor(() =>
      expect(screen.getByTestId('loc')).toHaveTextContent('/manage/distributors'),
    );
  });
});
