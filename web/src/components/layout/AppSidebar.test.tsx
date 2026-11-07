import { useCompanyStore } from '@/stores/company-store';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useLocation } from 'react-router';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { AppSidebar } from './AppSidebar';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { api } from '@/lib/api';
import { useBrandStore } from '@/stores/brand-store';
import { ORG_CHART_MIN_AGENTS } from '@/lib/nav-visibility';

/** Minimal agent fixtures — just enough for `agents.length` (D6 org-chart gate). */
function makeAgents(count: number) {
  return Array.from({ length: count }, (_, i) => ({
    name: `agent-${i}`,
    display_name: `Agent ${i}`,
    status: 'active',
  }));
}

function renderSidebar() {
  return renderWithProviders(
    <SidebarProvider>
      <AppSidebar />
    </SidebarProvider>,
  );
}

/** True when `a` precedes `b` in document order. */
function precedes(a: Element, b: Element): boolean {
  return Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.spyOn(api.agents, 'callable').mockImplementation(async () => ({ agents: useAgentsStore.getState().agents.filter(a => useAuthStore.getState().bindings.some(b => b.agent_name === a.name)).map(a => ({ ...a, via: 'binding', selectable: true })), unrestricted: useAuthStore.getState().user?.role === 'admin', default_agent: null }) as never);
  localStorage.clear();
  useBrandStore.setState({ brand: null });
  useCompanyStore.getState().setCompany(null);
  vi.spyOn(api.about, 'get').mockResolvedValue({} as never);
  useAuthStore.setState({
    user: { display_name: 'Boss', role: 'admin' } as never,
    bindings: [],
  } as never);
  useCommandPaletteStore.setState({ open: false });
  // Default to the non-personal layout; the personal-IA test opts in itself.
  useSystemStore.setState({ status: null } as never);
  // Default to no agents; the org-chart-disclosure tests opt into a roster.
  useAgentsStore.setState({ agents: [] as never, fetchAgents: vi.fn() as never });
  useApprovalsStore.setState({ pendingCount: 0, stale: true });
});

describe('AppSidebar (Multica shell)', () => {
  it('shows the company identity while preserving the distributor fallback', () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner Desk', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    const logo = `/api/company/logo/${'a'.repeat(64)}.png`;
    useCompanyStore.getState().setCompany({ name: 'Acme Team', logo_url: logo });
    renderSidebar();
    expect(screen.getByRole('img', { name: 'Acme Team' })).toHaveAttribute('src', logo);
    expect(screen.getByText('Acme Team')).toBeInTheDocument();
    expect(screen.queryByText('Partner Desk')).toBeNull();
  });

  it('uses the distributor logo when no customer logo is configured', () => {
    const logo = 'data:image/png;base64,iVBORw0KGgo=';
    useBrandStore.getState().setBrand({ product_name: 'Partner Desk', logo_light: logo });
    renderSidebar();
    expect(screen.getByRole('img', { name: 'Partner Desk' })).toHaveAttribute('src', logo);
    expect(screen.getByText('Partner Desk')).toBeInTheDocument();
  });

  // Multica IA (spec §5.1): flat daily row + 工作 / 公司 / 設定 groups + a live
  // 員工 zone. Home is the single spine.
  it('renders the flat daily items and the three collapsible group labels', () => {
    renderSidebar();
    // Flat daily items (no group header), plus the footer Inbox bell shortcut.
    expect(screen.getByRole('link', { name: /Dashboard/i })).toBeInTheDocument();
    expect(screen.getAllByRole('link', { name: /Inbox/i }).length).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: /New chat/i })).toBeInTheDocument();
    // 對話紀錄 zone (moved above 設定 on 2026-08-04, WP18-B).
    expect(screen.getByText(/^Conversations$/)).toBeInTheDocument();
    // The three group labels.
    expect(screen.getByText(/^Work$/)).toBeInTheDocument();
    expect(screen.getByText(/^Company$/)).toBeInTheDocument();
    expect(screen.getByText(/^Settings$/)).toBeInTheDocument();
    // The primary 交辦 action + the ⌘K search trigger.
    expect(screen.getByRole('button', { name: /New chat/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Command palette/i })).toBeInTheDocument();
  });

  it.each([
    { stale: true, pendingCount: 0, badge: '?', label: 'Inbox (Data may be stale)' },
    { stale: true, pendingCount: 7, badge: '?', label: 'Inbox (Data may be stale)' },
    { stale: false, pendingCount: 0, badge: '', label: 'Inbox' },
    { stale: false, pendingCount: 7, badge: '7', label: 'Inbox (7 need you)' },
  ])('keeps Inbox identifiable with stale=$stale and count=$pendingCount', ({ stale, pendingCount, badge, label }) => {
    useApprovalsStore.setState({ stale, pendingCount });
    renderSidebar();
    const inbox = screen.getByRole('link', { name: /Inbox/i });
    expect(inbox).toHaveAccessibleName(label);
    expect(inbox).toHaveAttribute('href', '/inbox');
    expect(inbox).toHaveAttribute('title', label);
    expect(inbox.textContent).toBe(badge);
  });

  it.each(['employee', 'manager'])('hides admin-gated items for %s (fail-closed UX)', (role) => {
    useAuthStore.setState({ user: { display_name: 'U', role } as never });
    renderSidebar();
    if (role === 'employee') {
      expect(screen.queryByRole('link', { name: /Reports/i })).not.toBeInTheDocument();
      expect(screen.queryByRole('link', { name: /Manage/i })).not.toBeInTheDocument();
    }
    // …while open surfaces stay.
    expect(screen.getByRole('link', { name: /Dashboard/i })).toBeInTheDocument();
    if (role === 'manager') expect(screen.getByRole('link', { name: /About/i })).toBeInTheDocument();
    else expect(screen.queryByRole('link', { name: /About/i })).not.toBeInTheDocument();
    // Employees get the scoped 1.80.0 automation page; managers retain the admin gate.
    if (role === 'employee') expect(screen.getByRole('link', { name: /Automations/i })).toHaveAttribute('href', '/routines');
    else expect(screen.queryByRole('link', { name: /Automations/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Gallery/i })).not.toBeInTheDocument();
  });

  // D6 (09-edition-split-features.md §4): the org chart is a progressive-
  // disclosure surface now, not a `personalHidden` one — it waits for the
  // roster to clear ORG_CHART_MIN_AGENTS, on EITHER edition.
  it('org chart discloses itself once the roster reaches ORG_CHART_MIN_AGENTS', async () => {
    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS - 1) as never });
    const { unmount } = renderSidebar();
    expect(screen.queryByRole('link', { name: /^Team$/i })).not.toBeInTheDocument();
    unmount();

    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS) as never });
    renderSidebar();
    expect(await screen.findByRole('link', { name: /^Team$/i })).toBeInTheDocument();
  });

  it('personal edition: org chart is folded into 進階, not shown before the roster is big enough', async () => {
    const user = userEvent.setup();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    useAgentsStore.setState({ agents: makeAgents(1) as never });
    renderSidebar();
    await user.click(screen.getByText(/^Advanced$/));
    expect(screen.queryByRole('link', { name: /^Team$/i })).not.toBeInTheDocument();
  });

  it('marks the current route active (aria-current + accent class)', () => {
    // Default MemoryRouter route is '/', so Dashboard is the active spine.
    renderSidebar();
    const home = screen.getByRole('link', { name: /Dashboard/i });
    expect(home).toHaveAttribute('aria-current', 'page');
    expect(home.className).toContain('bg-sidebar-accent');
    // A non-active item carries neither.
    const chat = screen.getByRole('button', { name: /New chat/i });
    expect(chat).not.toHaveAttribute('aria-current', 'page');
  });

  // Personal IA (2026-07-29 client feedback): flat 主區 (daily + Automations /
  // World / Skills / Memory), no 工作/公司 groups, no staff zone; 設定 and
  // 進階 close the rail collapsed. Roster/org/pet-studio hidden (pet studio is
  // desktop-only and jsdom is not Tauri). 2026-07-30: the knowledge base
  // merged into 記憶, so Memory took Knowledge's primary-rail slot.
  it('personal edition: minimal primary rail, collapsed 設定/進階, staff surfaces hidden', async () => {
    const user = userEvent.setup();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderSidebar();

    // 主區 flat items are present.
    expect(screen.getByRole('link', { name: /Automations/i })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Memory/i })).toBeInTheDocument();
    // Enterprise group labels are gone; 進階 exists but starts collapsed.
    expect(screen.queryByText(/^Work$/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Company$/)).not.toBeInTheDocument();
    expect(screen.getByText(/^Advanced$/)).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Task Board/i })).not.toBeInTheDocument();
    // 設定 collapsed by default too.
    expect(screen.queryByRole('link', { name: /About/i })).not.toBeInTheDocument();
    // AI 員工 is back on Personal too (2026-08-04, D11 overturns the
    // 2026-07-29 decision to hide the roster).
    expect(screen.getByRole('link', { name: /^Agents$/i })).toBeInTheDocument();
    // WP14 client-annotated order: 新對話 → 例行工作 → 技能庫 → 記憶 → AI 員工
    // → 世界. Read straight off the rendered rail, so a reshuffle in
    // `nav-model` cannot silently drift from what the client signed off.
    const order = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    expect(precedes(screen.getByRole('button', { name: /New chat/i }), screen.getByRole('link', { name: /Automations/i }))).toBe(true);
    const seq = ['Automations', 'Skills', 'Memory', 'Agents', 'Virtual office'];
    const positions = seq.map((l) => order.indexOf(l));
    // Every one of the six must actually be on the rail — otherwise a -1 would
    // let this assertion pass while the row was missing entirely.
    expect(positions.every((i) => i >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    // WP18-B: 對話紀錄 sits AFTER 世界 and BEFORE 設定 — never in front of the
    // six primary surfaces, which is where it used to be.
    const conversations = screen.getByText(/^Conversations$/);
    expect(precedes(screen.getByRole('link', { name: /Virtual office/i }), conversations)).toBe(true);
    expect(precedes(conversations, screen.getByText(/^Settings$/))).toBe(true);
    // Still hidden on Personal: pet studio (desktop-only, and jsdom is not Tauri).
    expect(screen.queryByRole('link', { name: /Pet studio/i })).not.toBeInTheDocument();

    // P2-a-nav (2026-08-15): AI Teams (formerly "Expert Packs", admin-only)
    // is promoted to the primary rail — visible without expanding 進階, and
    // exactly once (the design doc explicitly warns "只加不移會重複出現").
    expect(screen.getAllByRole('link', { name: /^AI Teams$/i })).toHaveLength(1);

    // Expanding 進階 reveals the folded work surfaces.
    await user.click(screen.getByText(/^Advanced$/));
    expect(screen.queryByRole('link', { name: /Task Board/i })).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Reports/i })).toBeInTheDocument();
    // AI Teams must NOT also appear in the expanded 進階 section — it left
    // `personalAdvancedGroup` for `personalPrimaryItems` (P2-a-nav).
    expect(screen.getAllByRole('link', { name: /^AI Teams$/i })).toHaveLength(1);

    // WP-NAV (2026-08-12): the 進階 group reads work cluster → company cluster,
    // the same sequence the Enterprise 工作 / 公司 groups use, so the two
    // editions never present these surfaces in two different orders. 成長 leads
    // the company cluster (高頻・低重要 on the matrix), 組織架構 follows it.
    const advanced = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    const advSeq = ['Reports', 'OS', 'Growth', 'Widget Studio'];
    const advPos = advSeq.map((l) => advanced.indexOf(l));
    expect(advPos.every((i) => i >= 0)).toBe(true);
    expect(advPos).toEqual([...advPos].sort((a, b) => a - b));
  });

  // WP14 (2026-08-04) — the two editions must present the same six surfaces in
  // the same order. This is the Enterprise half of the pair; the Personal half
  // is asserted in the personal-IA case above, using the same rendered-order
  // read so neither can drift without the other failing.
  it('enterprise edition: 公司 group follows the same 技能庫→記憶→AI 員工→世界 order', async () => {
    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    // D6: 'Team' (the org chart) only shows once the roster clears the
    // progressive-disclosure threshold — this assertion is about ordering,
    // not disclosure, so seed enough agents for it to actually be present.
    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS) as never });
    renderSidebar();
    await screen.findByRole('link', { name: /^Team$/i });
    const order = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    // WP-NAV (2026-08-12) inserts 成長 between 世界 and the 低頻 tail: the
    // matrix grades it 高頻・低重要, so it sits with the daily surfaces but
    // still below the four the client annotated.
    const seq = ['Skills', 'Memory', 'Agents', 'Virtual office', 'Growth', 'Team'];
    const positions = seq.map((l) => order.indexOf(l));
    // -1 guard: a missing row would otherwise sneak through as "sorted".
    expect(positions.every((i) => i >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    // 例行工作 leads 工作 and 任務看板 closes it — the Enterprise stand-in for
    // folding the board into 進階.
    expect(order.indexOf('Automations')).toBeGreaterThanOrEqual(0);
    expect(order).not.toContain('Task Board');
    expect(order.indexOf('Automations')).toBeLessThan(order.indexOf('Reports'));
    // WP-NAV: the occasional rows sink below the oversight pair (時間軸 / 用量)
    // and stay above the deliberately-demoted board. `/forks` is
    // progressive-disclosure (no fork records in this fixture) so OS is the one
    // occasional row rendered here.
    expect(order.indexOf('OS')).toBeGreaterThan(order.indexOf('Reports'));
    // WP18-B: the 對話紀錄 slot is identical on Enterprise — after the 公司
    // group's last row, before 設定. If the two editions ever fork here, one of
    // these two assertions fails.
    const conversations = screen.getByText(/^Conversations$/);
    expect(precedes(screen.getByText(/^Company$/), conversations)).toBe(true);
    expect(precedes(screen.getByRole('link', { name: /Virtual office/i }), conversations)).toBe(true);
    expect(precedes(conversations, screen.getByText(/^Settings$/))).toBe(true);
  });

  // WP14 (2026-08-04): the footer cost chip is gone; the slot now answers
  // "which build am I on?" from the gateway's own `system.status`.
  it('shows the running version in the footer instead of a spend figure', () => {
    useSystemStore.setState({
      status: { edition_profile: 'enterprise', version: '1.51.0' } as never,
    });
    renderSidebar();
    expect(screen.getByText('v1.51.0')).toBeInTheDocument();
    // No dollar figure anywhere in the rail any more.
    expect(screen.queryByText(/^\$\d/)).not.toBeInTheDocument();
  });

  // Before the gateway handshake lands, `system.status` is null. The footer must
  // stay silent rather than render a bare "v" or the literal "undefined" — the
  // classic shape of this bug.
  it('renders no version text at all while the gateway status is unknown', () => {
    useSystemStore.setState({ status: null } as never);
    const { container } = renderSidebar();
    expect(screen.queryByText(/^v\d/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^v$/)).not.toBeInTheDocument();
    expect(container.textContent).not.toContain('undefined');
    expect(container.textContent).not.toContain('NaN');
    // The rest of the footer card still renders — only the version line is gone.
    expect(screen.getByText('Lv —')).toBeInTheDocument();
  });

  it('opens the command palette from the search trigger', async () => {
    const user = userEvent.setup();
    renderSidebar();
    expect(useCommandPaletteStore.getState().open).toBe(false);
    await user.click(screen.getByRole('button', { name: /Command palette/i }));
    expect(useCommandPaletteStore.getState().open).toBe(true);
  });
});

// ── 新功能 chip convention (2026-08-14) ─────────────────────────────────
import { isNewFeature } from './nav-model';

describe('isNewFeature (新功能 chip expiry)', () => {
  it('shows while running version is at or below the shipping minor', () => {
    expect(isNewFeature('1.58.0', '1.57.0')).toBe(true); // pre-release build
    expect(isNewFeature('1.58.0', '1.58.0')).toBe(true); // the shipping minor
    expect(isNewFeature('1.58.0', '1.58.3')).toBe(true); // patches never expire it
    expect(isNewFeature('1.58.0', 'v1.58.1')).toBe(true); // tolerant of v-prefix
  });

  it('expires on the next minor and stays inert without a tag', () => {
    expect(isNewFeature('1.58.0', '1.59.0')).toBe(false);
    expect(isNewFeature('1.58.0', '2.0.0')).toBe(false);
    expect(isNewFeature(undefined, '1.57.0')).toBe(false);
  });

  it('keeps the chip when the running version is unknown', () => {
    expect(isNewFeature('1.58.0', null)).toBe(true);
    expect(isNewFeature('garbage', '1.57.0')).toBe(false);
  });
});


describe('employee sidebar', () => {
  beforeEach(() => {
    useSystemStore.setState({ status: { edition_profile: 'enterprise', version: '1.79.0' } as never });
    useAuthStore.setState({ user: { id: 'employee', display_name: 'Alice', role: 'employee', email: 'alice@example.com', department: 'Sales' } as never });
    useBrandStore.setState({ brand: null });
  useCompanyStore.getState().setCompany(null);
    vi.spyOn(api.personalGoogle, 'status').mockRejectedValue(new Error('offline'));
  });

  it('has an inline inbox and no footer bell, edition card or clickable brand', async () => {
    renderSidebar();
    expect(document.querySelector('svg.lucide-bell')).toBeNull();
    expect(screen.queryByTitle('Enterprise')).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Inbox/i })).toHaveAttribute('href', '/inbox');
    expect(screen.queryByRole('button', { name: /Workspace/i })).not.toBeInTheDocument();
    expect(screen.getByText('Sales')).toBeInTheDocument();
    expect(screen.getAllByRole('link').slice(0, 7).map(link => link.getAttribute('href'))).toEqual(['/', '/inbox', '/routines', '/skills', '/memory', '/world', '/org']);
    expect(precedes(screen.getByRole('button', { name: /Command palette/i }), screen.getByRole('button', { name: /New chat/i }))).toBe(true);
    expect(screen.queryByRole('link', { name: /Task Board/i })).not.toBeInTheDocument();
    await waitFor(() => expect(api.personalGoogle.status).toHaveBeenCalledTimes(1));
  });

  it('opens the account menu by keyboard and quietly handles a failed connection probe', async () => {
    const user = userEvent.setup();
    renderSidebar();
    const trigger = screen.getByRole('button', { name: 'Alice' });
    trigger.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText('alice@example.com')).toBeInTheDocument();
    for (const name of ['My connections', 'Settings', 'Appearance', 'Get help']) {
      expect(screen.getByRole('menuitem', { name: new RegExp(name, 'i') })).toBeInTheDocument();
    }
    expect(screen.getByRole('menuitem', { name: 'Switch language' })).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: '日本語' })).not.toBeInTheDocument();
    expect(screen.getByRole('menuitem', { name: /Sign Out/i })).toBeInTheDocument();
    await waitFor(() => expect(api.personalGoogle.status).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole('img', { name: 'Connection needed' })).not.toBeInTheDocument();
    await user.keyboard('{Escape}');
    await user.click(trigger);
    await waitFor(() => expect(api.personalGoogle.status).toHaveBeenCalledTimes(3));
  });

  it('preserves the admin and Personal footer bell', () => {
    useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' } as never });
    const first = renderSidebar();
    expect(document.querySelector('svg.lucide-bell')).not.toBeNull();
    first.unmount();
    useAuthStore.setState({ user: { display_name: 'Alice', role: 'employee' } as never });
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderSidebar();
    expect(document.querySelector('svg.lucide-bell')).not.toBeNull();
  });

  it('marks and sorts the primary employee before an Owner binding', async () => {
    useAgentsStore.setState({ agents: makeAgents(2) as never });
    useAuthStore.setState({ user: { id: 'employee', role: 'employee', primary_agent_id: 'agent-1' } as never,
      bindings: [{ agent_name: 'agent-0', access_level: 'owner' }, { agent_name: 'agent-1', access_level: 'operator' }] as never });
    renderSidebar();
    await screen.findByText('Agent 0');
    const links = screen.getAllByRole('link').filter(link => /^\/agents\//.test(link.getAttribute('href') ?? ''));
    expect(links.map(link => link.getAttribute('href'))).toEqual(['/agents/agent-1', '/agents/agent-0']);
    expect(links[0]).toHaveTextContent('Primary');
    expect(screen.getAllByRole('button', { name: 'Set as primary' })).toHaveLength(2);
  });

  it('sets the primary from the row menu and refreshes the current user before updating staff', async () => {
    const user = userEvent.setup();
    const bindings = [
      { agent_name: 'agent-0', access_level: 'owner' },
      { agent_name: 'agent-1', access_level: 'operator' },
    ];
    const currentUser = { id: 'employee', role: 'employee', display_name: 'Alice', primary_agent_id: 'agent-0' };
    useAgentsStore.setState({ agents: makeAgents(2) as never });
    useAuthStore.setState({ user: currentUser as never, bindings: bindings as never });
    let finishRefresh!: (value: unknown) => void;
    const refreshing = new Promise(resolve => { finishRefresh = resolve; });
    const me = vi.spyOn(api.users, 'me').mockImplementation(() => refreshing as ReturnType<typeof api.users.me>);
    renderSidebar();
    const staffLinks = () => screen.getAllByRole('link').filter(link => /^\/agents\//.test(link.getAttribute('href') ?? ''));
    await waitFor(() => expect(staffLinks()[0]).toHaveAttribute('href', '/agents/agent-0'));
    await user.click(screen.getAllByRole('button', { name: 'Set as primary' })[1]);
    await user.click(await screen.findByRole('menuitem', { name: 'Set as primary' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.set_primary_agent', { agent_id: 'agent-1' }));
    await waitFor(() => expect(me).toHaveBeenCalledOnce());
    // The mutation alone does not invent a new local current-user state.
    expect(staffLinks()[0]).toHaveAttribute('href', '/agents/agent-0');
    finishRefresh({ user: { ...currentUser, primary_agent_id: 'agent-1' }, bindings });
    await waitFor(() => {
      expect(staffLinks()[0]).toHaveAttribute('href', '/agents/agent-1');
      expect(staffLinks()[0]).toHaveTextContent('Primary');
      expect(staffLinks()[1]).not.toHaveTextContent('Primary');
    });
    expect(useAuthStore.getState().user?.primary_agent_id).toBe('agent-1');
    me.mockRestore();
  });

  it('does not offer a primary selection for Viewer bindings', async () => {
    useAgentsStore.setState({ agents: makeAgents(2) as never });
    useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never,
      bindings: [{ agent_name: 'agent-0', access_level: 'viewer' }, { agent_name: 'agent-1', access_level: 'operator' }] as never });
    renderSidebar();
    await waitFor(() => expect(screen.getAllByRole('button', { name: 'Set as primary' })).toHaveLength(1));
  });

  it('sorts bound staff by owner, operator, viewer and limits the list to eight', async () => {
    const agents = makeAgents(10);
    useAgentsStore.setState({ agents: agents as never });
    useAuthStore.setState({ bindings: agents.map((a, index) => ({ agent_name: a.name, access_level: index === 9 ? 'owner' : index === 8 ? 'operator' : 'viewer' })) as never });
    renderSidebar();
    await screen.findByText('Agent 0');
    const links = screen.getAllByRole('link').filter(link => /^\/agents\//.test(link.getAttribute('href') ?? ''));
    expect(links).toHaveLength(8);
    expect(links.slice(0, 2).map(link => link.getAttribute('href'))).toEqual(['/agents/agent-9', '/agents/agent-8']);
    expect(screen.getByRole('link', { name: /View all/i })).toHaveAttribute('href', '/agents');
  });
});


it('shows help even when a distributor brand has no support notice', async () => {
  useAuthStore.setState({ user: { display_name: 'Alice', role: 'employee' } as never });
  useBrandStore.setState({ brand: { product_name: 'Partner', logo_light: '' } });
  vi.spyOn(api.personalGoogle, 'status').mockResolvedValue({
    connection: null, requests: [], integration_enabled: true, requires_login: false,
  } as never);
  const user = userEvent.setup();
  renderSidebar();
  await user.click(screen.getByRole('button', { name: 'Alice' }));
  expect(await screen.findByRole('img', { name: 'Connection needed' })).toBeInTheDocument();
  expect(screen.getByRole('menuitem', { name: 'Get help' })).toBeInTheDocument();
});


describe('single desktop new-conversation entry', () => {
  it.each(['admin', 'manager', 'employee'] as const)('shows one entry and no task links for %s, regardless of employee_tasks', role => {
    useAuthStore.setState({ user: { id: 'u1', role } as never });
    useSystemStore.setState({ status: { edition_profile: 'enterprise', employee_tasks: true } as never });
    const { container } = renderSidebar();
    expect(screen.getAllByRole('button', { name: 'New chat' })).toHaveLength(1);
    expect(screen.queryByRole('link', { name: 'New chat' })).not.toBeInTheDocument();
    expect(container.querySelector('a[href="/tasks"], a[href="/goals"], a[href="/assign"]')).toBeNull();
  });
});

function MenuLocation() {
  const location = useLocation();
  return <output data-testid="menu-location">{location.pathname}{location.hash}</output>;
}

it('opens the distributor support entry when the server provides a notice', async () => {
  useAuthStore.setState({ user: { id: 'employee', display_name: 'Alice', role: 'employee' } as never });
  vi.spyOn(api.personalGoogle, 'status').mockRejectedValue(new Error('offline'));
  vi.spyOn(api.about, 'get').mockResolvedValue({ distributor_notice: '<p>Support</p>' } as never);
  const user = userEvent.setup();
  renderWithProviders(<SidebarProvider><AppSidebar /><MenuLocation /></SidebarProvider>);
  await waitFor(() => expect(api.about.get).toHaveBeenCalled());
  await user.click(screen.getByRole('button', { name: 'Alice' }));
  await user.click(await screen.findByRole('menuitem', { name: 'Get help' }));
  expect(screen.getByTestId('menu-location')).toHaveTextContent('/about#distributor-support');
});

it('opens language settings from the employee menu and keeps the version out of the rail', async () => {
  useAuthStore.setState({ user: { id: 'employee', display_name: 'Alice', role: 'employee' } as never });
  useSystemStore.setState({ status: { edition_profile: 'enterprise', version: '1.80.0' } as never });
  vi.spyOn(api.personalGoogle, 'status').mockRejectedValue(new Error('offline'));
  const user = userEvent.setup();
  renderWithProviders(<SidebarProvider><AppSidebar /><MenuLocation /></SidebarProvider>);
  expect(screen.queryByText('v1.80.0')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'Alice' }));
  await user.click(await screen.findByRole('menuitem', { name: 'Switch language' }));
  expect(screen.getByTestId('menu-location')).toHaveTextContent('/me/account#language');
});

import { callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';

it('does not set a primary employee revoked while its menu is open', async () => {
  const user = userEvent.setup();
  useAgentsStore.setState({ agents: makeAgents(1) as never });
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never,
    bindings: [{ agent_name: 'agent-0', access_level: 'owner' }] as never });
  renderSidebar();
  await user.click(await screen.findByRole('button', { name: 'Set as primary' }));
  const item = await screen.findByRole('menuitem', { name: 'Set as primary' });
  await waitFor(() => expect(item).not.toHaveAttribute('aria-disabled', 'true'));
  act(() => {
    const key = JSON.stringify([callableSubject(), null]);
    useCallableAgentsStore.setState({ entries: { [key]: { agents: [], unrestricted: false, loading: false, confirmedAt: Date.now() + 1 } } });
    fireEvent.click(item);
  });
  expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.set_primary_agent')).toBe(false);
});
