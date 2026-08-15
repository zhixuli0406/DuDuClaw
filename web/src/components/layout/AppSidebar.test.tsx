import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { AppSidebar } from './AppSidebar';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useAgentsStore } from '@/stores/agents-store';
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
  localStorage.clear();
  useAuthStore.setState({
    user: { display_name: 'Boss', role: 'admin' } as never,
    bindings: [],
  } as never);
  useCommandPaletteStore.setState({ open: false });
  // Default to the non-personal layout; the personal-IA test opts in itself.
  useSystemStore.setState({ status: null } as never);
  // Default to no agents; the org-chart-disclosure tests opt into a roster.
  useAgentsStore.setState({ agents: [] as never, fetchAgents: vi.fn() as never });
});

describe('AppSidebar (Multica shell)', () => {
  // Multica IA (spec §5.1): flat daily row + 工作 / 公司 / 設定 groups + a live
  // 員工 zone. Home is the single spine.
  it('renders the flat daily items and the three collapsible group labels', () => {
    renderSidebar();
    // Flat daily items (no group header). Inbox appears twice (the nav row +
    // the footer bell shortcut), so assert at least one.
    expect(screen.getByRole('link', { name: /Dashboard/i })).toBeInTheDocument();
    expect(screen.getAllByRole('link', { name: /Inbox/i }).length).toBeGreaterThan(0);
    expect(screen.getByRole('link', { name: /New chat/i })).toBeInTheDocument();
    // 對話紀錄 zone (moved above 設定 on 2026-08-04, WP18-B).
    expect(screen.getByText(/^Conversations$/)).toBeInTheDocument();
    // The three group labels.
    expect(screen.getByText(/^Work$/)).toBeInTheDocument();
    expect(screen.getByText(/^Company$/)).toBeInTheDocument();
    expect(screen.getByText(/^Settings$/)).toBeInTheDocument();
    // The primary 交辦 action + the ⌘K search trigger.
    expect(screen.getByRole('button', { name: /New task/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Command palette/i })).toBeInTheDocument();
  });

  it('hides role-gated items below the required role (fail-closed UX)', () => {
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    renderSidebar();
    // manager+ surfaces are hidden for an employee…
    expect(screen.queryByRole('link', { name: /Reports/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Manage/i })).not.toBeInTheDocument();
    // …while open surfaces stay.
    expect(screen.getByRole('link', { name: /Dashboard/i })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /About/i })).toBeInTheDocument();
    // D7 (09-edition-split-features.md §4): 例行工作 dropped its manager+ gate
    // — an employee's own agent's cron schedule is the same "own scope" as
    // 共同計畫, which was already open.
    expect(screen.getByRole('link', { name: /Routines/i })).toBeInTheDocument();
  });

  // D6 (09-edition-split-features.md §4): the org chart is a progressive-
  // disclosure surface now, not a `personalHidden` one — it waits for the
  // roster to clear ORG_CHART_MIN_AGENTS, on EITHER edition.
  it('org chart discloses itself once the roster reaches ORG_CHART_MIN_AGENTS', () => {
    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS - 1) as never });
    const { unmount } = renderSidebar();
    expect(screen.queryByRole('link', { name: /^Team$/i })).not.toBeInTheDocument();
    unmount();

    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS) as never });
    renderSidebar();
    expect(screen.getByRole('link', { name: /^Team$/i })).toBeInTheDocument();
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
    const chat = screen.getByRole('link', { name: /New chat/i });
    expect(chat).not.toHaveAttribute('aria-current', 'page');
  });

  // Personal IA (2026-07-29 client feedback): flat 主區 (daily + Routines /
  // World / Skills / Memory), no 工作/公司 groups, no staff zone; 設定 and
  // 進階 close the rail collapsed. Roster/org/pet-studio hidden (pet studio is
  // desktop-only and jsdom is not Tauri). 2026-07-30: the knowledge base
  // merged into 記憶, so Memory took Knowledge's primary-rail slot.
  it('personal edition: minimal primary rail, collapsed 設定/進階, staff surfaces hidden', async () => {
    const user = userEvent.setup();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderSidebar();

    // 主區 flat items are present.
    expect(screen.getByRole('link', { name: /Routines/i })).toBeInTheDocument();
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
    const seq = ['New chat', 'Routines', 'Skills', 'Memory', 'Agents', 'World'];
    const positions = seq.map((l) => order.indexOf(l));
    // Every one of the six must actually be on the rail — otherwise a -1 would
    // let this assertion pass while the row was missing entirely.
    expect(positions.every((i) => i >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    // WP18-B: 對話紀錄 sits AFTER 世界 and BEFORE 設定 — never in front of the
    // six primary surfaces, which is where it used to be.
    const conversations = screen.getByText(/^Conversations$/);
    expect(precedes(screen.getByRole('link', { name: /World/i }), conversations)).toBe(true);
    expect(precedes(conversations, screen.getByText(/^Settings$/))).toBe(true);
    // Still hidden on Personal: pet studio (desktop-only, and jsdom is not Tauri).
    expect(screen.queryByRole('link', { name: /Pet studio/i })).not.toBeInTheDocument();

    // P2-a-nav (2026-08-15): AI Teams (formerly "Expert Packs", admin-only)
    // is promoted to the primary rail — visible without expanding 進階, and
    // exactly once (the design doc explicitly warns "只加不移會重複出現").
    expect(screen.getAllByRole('link', { name: /^AI Teams$/i })).toHaveLength(1);

    // Expanding 進階 reveals the folded work surfaces.
    await user.click(screen.getByText(/^Advanced$/));
    expect(screen.getByRole('link', { name: /Task Board/i })).toBeInTheDocument();
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
    const advSeq = ['Task Board', 'Reports', 'OS', 'Growth', 'Widget Studio'];
    const advPos = advSeq.map((l) => advanced.indexOf(l));
    expect(advPos.every((i) => i >= 0)).toBe(true);
    expect(advPos).toEqual([...advPos].sort((a, b) => a - b));
  });

  // WP14 (2026-08-04) — the two editions must present the same six surfaces in
  // the same order. This is the Enterprise half of the pair; the Personal half
  // is asserted in the personal-IA case above, using the same rendered-order
  // read so neither can drift without the other failing.
  it('enterprise edition: 公司 group follows the same 技能庫→記憶→AI 員工→世界 order', () => {
    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    // D6: 'Team' (the org chart) only shows once the roster clears the
    // progressive-disclosure threshold — this assertion is about ordering,
    // not disclosure, so seed enough agents for it to actually be present.
    useAgentsStore.setState({ agents: makeAgents(ORG_CHART_MIN_AGENTS) as never });
    renderSidebar();
    const order = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    // WP-NAV (2026-08-12) inserts 成長 between 世界 and the 低頻 tail: the
    // matrix grades it 高頻・低重要, so it sits with the daily surfaces but
    // still below the four the client annotated.
    const seq = ['Skills', 'Memory', 'Agents', 'World', 'Growth', 'Team'];
    const positions = seq.map((l) => order.indexOf(l));
    // -1 guard: a missing row would otherwise sneak through as "sorted".
    expect(positions.every((i) => i >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
    // 例行工作 leads 工作 and 任務看板 closes it — the Enterprise stand-in for
    // folding the board into 進階.
    expect(order.indexOf('Routines')).toBeGreaterThanOrEqual(0);
    expect(order.indexOf('Task Board')).toBeGreaterThan(order.indexOf('Routines'));
    expect(order.indexOf('Routines')).toBeLessThan(order.indexOf('Reports'));
    expect(order.indexOf('Task Board')).toBeGreaterThan(order.indexOf('Reports'));
    // WP-NAV: the occasional rows sink below the oversight pair (時間軸 / 用量)
    // and stay above the deliberately-demoted board. `/forks` is
    // progressive-disclosure (no fork records in this fixture) so OS is the one
    // occasional row rendered here.
    expect(order.indexOf('OS')).toBeGreaterThan(order.indexOf('Reports'));
    expect(order.indexOf('OS')).toBeLessThan(order.indexOf('Task Board'));
    // WP18-B: the 對話紀錄 slot is identical on Enterprise — after the 公司
    // group's last row, before 設定. If the two editions ever fork here, one of
    // these two assertions fails.
    const conversations = screen.getByText(/^Conversations$/);
    expect(precedes(screen.getByText(/^Company$/), conversations)).toBe(true);
    expect(precedes(screen.getByRole('link', { name: /World/i }), conversations)).toBe(true);
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
