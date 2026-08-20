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

function setRole(role: 'admin' | 'manager' | 'employee') {
  useAuthStore.setState({
    user: { id: 'u1', username: 'owner', display_name: 'Owner', role },
  } as never);
}

// N-3 (2026-08): unlike the pre-existing redirect tests above (which only
// ever land on simple targets — /inbox, a settings tab, /welcome, /), the
// new "系統設定 app relocation" describe block below actually mounts the six
// migrated pages for real (a redirect's whole point is to land somewhere
// that RENDERS). A couple of them fire RPCs the generic `{agents, tasks,
// events}` fallback below answers with the WRONG shape instead of failing —
// SecurityPage's killswitch card and HomePage's widget-catalog/subordinates
// effects all wrap their fetch in their own `.catch(() => <safe default>)`,
// so rejecting (not resolving-wrong-shaped) is what actually exercises that
// fallback instead of crashing on `undefined.<field>`.
const REJECTS_TO_SAFE_DEFAULT = new Set([
  'killswitch.get',
  'dashboard.widgets.catalog',
  'dashboard.layout.get',
  'widgets.custom.list',
  'users.subordinates',
]);

beforeEach(() => {
  vi.clearAllMocks();
  // The whole app shell mounts here, so a few background widgets fire their own
  // RPCs. A benign shape covers the list-shaped ones; `growth.snapshot` gets an
  // explicit empty answer because its store dereferences the payload.
  mockWsClient.call.mockImplementation((method: string) =>
    method === 'growth.snapshot'
      ? Promise.resolve(null)
      : REJECTS_TO_SAFE_DEFAULT.has(method)
        ? Promise.reject(new Error(`mock: ${method} not implemented in this fixture`))
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

// N-1 (`DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-1, §2 L2): the new
// /app/:appId/* deep-link seam. No page has moved — every case below redirects
// into an existing, unchanged route, through that route's own (unchanged)
// guard chain. These are additive tests over an additive route: none of the
// other routes/redirects above are touched.
describe('App routes — /app/:appId/* deep-link seam (N-1)', () => {
  it('redirects /app/<id>/<rest> to the existing canonical route at /<rest>', async () => {
    await expectLandsOn('/app/workbench/goals', '/goals');
  });

  it('redirects the bare /app/<id> (no rest) to that app\'s default path', async () => {
    await expectLandsOn('/app/workbench', '/chat');
  });

  it('falls back to / for an unknown app id (fails closed, never guesses)', async () => {
    await expectLandsOn('/app/not-a-real-app/whatever', '/');
  });

  it('preserves query strings on the redirect target', async () => {
    await expectLandsOn('/app/memory/memory?tab=wiki', '/memory?tab=wiki');
  });

  it('the redirect target still enforces its own RoleGuard — an employee opening the admin-gated "comms" app default path bounces to /', async () => {
    setRole('employee');
    await expectLandsOn('/app/comms', '/');
  });

  it('an admin opening the same app lands on its real default path', async () => {
    setRole('admin');
    await expectLandsOn('/app/comms', '/manage/channels');
  });
});

// N-3 (`DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3, §2 L3): the six
// 系統設定 pages physically relocated to `/app/system/*` — unlike N-1's
// deep-link seam above, these are real page moves. Every OLD path (both the
// former `/manage/*` locations and the older top-level legacy aliases) must
// now redirect to the NEW canonical route instead of rendering the page
// directly, query string preserved.
describe('App routes — 系統設定 app relocation (N-3)', () => {
  it('/device → /app/system/device', async () => {
    await expectLandsOn('/device', '/app/system/device');
  });

  it('/manage/accounts → /app/system/accounts', async () => {
    await expectLandsOn('/manage/accounts', '/app/system/accounts');
  });

  it('/manage/updates → /app/system/updates', async () => {
    await expectLandsOn('/manage/updates', '/app/system/updates');
  });

  it('/manage/security → /app/system/security', async () => {
    await expectLandsOn('/manage/security', '/app/system/security');
  });

  it('/manage/license → /app/system/license', async () => {
    await expectLandsOn('/manage/license', '/app/system/license');
  });

  it('/manage/system → /app/system/settings (renamed to avoid colliding with the app id)', async () => {
    await expectLandsOn('/manage/system', '/app/system/settings');
  });

  // Split one test per alias (rather than four `expectLandsOn` calls in one
  // `it`, per this file's own convention) — `renderAppAt` mounts the whole
  // `<App/>` tree and nothing here unmounts between calls, so stacking
  // several in one test leaves multiple full trees mounted at once.
  it('the older top-level legacy alias /accounts lands on the same new canonical route', async () => {
    await expectLandsOn('/accounts', '/app/system/accounts');
  });

  it('the older top-level legacy alias /security lands on the same new canonical route', async () => {
    await expectLandsOn('/security', '/app/system/security');
  });

  it('the older top-level legacy alias /settings lands on the same new canonical route', async () => {
    await expectLandsOn('/settings', '/app/system/settings');
  });

  it('the older top-level legacy alias /license lands on the same new canonical route', async () => {
    await expectLandsOn('/license', '/app/system/license');
  });

  it('/partner → /app/system/license directly (single hop, not through /manage/license)', async () => {
    await expectLandsOn('/partner', '/app/system/license');
  });

  it('preserves the query string across the redirect (WP-0 password-change deep link)', async () => {
    await expectLandsOn('/settings?tab=account', '/app/system/settings?tab=account');
  });

  it('the new canonical routes render directly — no further redirect', async () => {
    renderAppAt('/app/system/accounts');
    await waitFor(() => expect(screen.getByTestId('loc')).toHaveTextContent('/app/system/accounts'));
  });

  it('role gates carried over exactly: license stays reachable by a manager (not just admin)', async () => {
    setRole('manager');
    await expectLandsOn('/manage/license', '/app/system/license');
  });

  it('role gates carried over exactly: the other five stay admin-only — an employee bounces home', async () => {
    // employee, not manager: landing on `/` (HomePage) with a manager-role
    // session exercises an unrelated pre-existing HomePage subordinates-fetch
    // code path this file's generic RPC mock doesn't shape for — matching
    // the file's own established convention for this exact assertion (see
    // "an employee opening the admin-gated comms app default path bounces to
    // /" above).
    setRole('employee');
    await expectLandsOn('/app/system/accounts', '/');
  });

  it('the app registry default path now points at the settings-hub home, not /manage/system', async () => {
    await expectLandsOn('/app/system', '/app/system');
    // Renders the hub itself (SystemHomePage), not a further bounce — the
    // page header title is the same `app.system.name` string the launcher
    // card and sidebar breadcrumb reuse.
    await waitFor(() => expect(screen.getByText(en['app.system.name'])).toBeInTheDocument());
  });
});
