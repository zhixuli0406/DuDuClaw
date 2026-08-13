import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { ManageShell } from './ManageShell';
import { manageAdvancedNav, allManageNav } from './nav-model';

/** Render ManageShell with a real nested-route tree (NavLink + Outlet need it —
 *  Zone D is real routing, not `?tab=`, per WP4.1). */
function renderManage(initialPath: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route path="/" element={<div>Home page</div>} />
          <Route path="manage" element={<ManageShell />}>
            {allManageNav.map((item) => (
              <Route
                key={item.to}
                path={item.to.replace('/manage/', '')}
                element={<div>{item.to} page</div>}
              />
            ))}
          </Route>
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({
    user: { display_name: 'Boss', role: 'admin' } as never,
    bindings: [],
  } as never);
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
});

describe('ManageShell (five-row rail, 2026-08-04 D18)', () => {
  it('renders the five primary entries in the client-decided order', () => {
    renderManage('/manage/channels');
    const links = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean);
    expect(links).toEqual([
      en['manage.channels'],
      en['manage.integrations'],
      en['manage.accounts'],
      en['manage.updates'],
      en['manage.advanced'],
    ]);
    // The former group labels are gone with the grouping itself.
    expect(screen.queryByText('Operations')).not.toBeInTheDocument();
    expect(screen.queryByText('Billing & licensing')).not.toBeInTheDocument();
  });

  it('unfolds the 進階設定 sub-list, and keeps 進階設定 lit, inside that subtree', () => {
    renderManage('/manage/billing');
    // Every folded surface is reachable from the rail once you are inside it.
    for (const item of manageAdvancedNav) {
      const label = en[item.label as keyof typeof en] as string;
      expect(screen.getByRole('link', { name: label })).toBeInTheDocument();
    }
    // The parent row stays highlighted so you can see where you are.
    const parent = screen.getByRole('link', { name: en['manage.advanced'] });
    expect(parent.className).toContain('bg-surface-selected');
  });

  it('keeps the rail five rows tall outside the 進階設定 subtree', () => {
    renderManage('/manage/channels');
    expect(screen.queryByRole('link', { name: en['manage.billing'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.system'] })).not.toBeInTheDocument();
  });

  it('marks the active route with aria-current and the selected-surface class', () => {
    renderManage('/manage/channels');
    const active = screen.getByRole('link', { name: en['manage.channels'] });
    expect(active).toHaveAttribute('aria-current', 'page');
    expect(active.className).toContain('bg-surface-selected');

    const inactive = screen.getByRole('link', { name: en['manage.integrations'] });
    expect(inactive).not.toHaveAttribute('aria-current', 'page');
    expect(inactive.className).toContain('text-muted-foreground');
  });

  it('renders the routed child page inside the content pane', () => {
    renderManage('/manage/accounts');
    expect(screen.getByText('/manage/accounts page')).toBeInTheDocument();
  });

  it('hides admin-gated items for a manager-only viewer', () => {
    useAuthStore.setState({ user: { display_name: 'M', role: 'manager' } as never });
    renderManage('/manage/billing');
    expect(screen.queryByRole('link', { name: en['manage.channels'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.system'] })).not.toBeInTheDocument();
    // 進階設定 is manager-visible, and so is what a manager may see under it.
    expect(screen.getByRole('link', { name: en['manage.advanced'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.billing'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.logs'] })).toBeInTheDocument();
  });

  it('hides enterprise-only items on the personal edition', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderManage('/manage/billing');
    expect(screen.queryByRole('link', { name: en['manage.governance'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.users'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.departments'] })).not.toBeInTheDocument();
    // Non-gated items in the same sub-list stay visible.
    expect(screen.getByRole('link', { name: en['manage.billing'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.inference'] })).toBeInTheDocument();
  });

  // D9 (09-edition-split-features.md §4, 2026-08-12) revises the 2026-08-04
  // call: 安全 and 日誌 are no longer `personalHidden` — a single operator
  // still needs an emergency brake and a way to see what happened, so both
  // stay in the rail (still folded under 進階設定, same as everything else
  // here). 可靠性 (an SRE-style fleet report) is the one that stays hidden.
  it('hides 可靠性 on the personal edition but keeps 安全 / 日誌 (D9), and keeps all three on enterprise', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    const { unmount } = renderManage('/manage/billing');
    expect(screen.queryByRole('link', { name: en['manage.reliability'] })).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.security'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.logs'] })).toBeInTheDocument();
    unmount();

    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    renderManage('/manage/billing');
    expect(screen.getByRole('link', { name: en['manage.security'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.reliability'] })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.logs'] })).toBeInTheDocument();
  });

  // 帳務 first, 設定 last — the client-annotated order of the advanced list.
  it('keeps the money surfaces above 設定 in the 進階設定 sub-list', () => {
    renderManage('/manage/billing');
    const labels = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    expect(labels.indexOf(en['manage.billing'])).toBeLessThan(labels.indexOf(en['manage.system']));
    expect(labels.indexOf(en['manage.license'])).toBeLessThan(labels.indexOf(en['manage.system']));
    expect(labels.at(-1)).toBe(en['manage.system']);
  });

  // WP-NAV (2026-08-12): 進階設定 IS the 進階層, sorted 低頻・高重要 →
  // 低頻・低重要 (12-ia-redesign-blueprint.md §2 over the frequency × importance
  // matrix). Asserted as an exact sequence, not a set of pairwise "before"
  // checks, so a future reshuffle has to come back here and restate the intent.
  it('orders the 進階設定 sub-list 錢 → 存取與安全 → 維運 → 設定', () => {
    renderManage('/manage/billing');
    const labels = screen
      .getAllByRole('link')
      .map((el) => el.textContent?.trim())
      .filter(Boolean) as string[];
    // Drop the five primary rows; what follows 進階設定 is the sub-list.
    const sub = labels.slice(labels.indexOf(en['manage.advanced']) + 1);
    expect(sub).toEqual([
      // 錢 — client order invariant (2026-08-04 WP14).
      en['manage.billing'],
      en['manage.license'],
      en['manage.distributors'],
      // 存取與安全 — 低頻・高重要.
      en['manage.security'],
      en['manage.governance'],
      en['manage.users'],
      en['manage.departments'],
      // 維運 — 低頻・低重要; 可靠性 and 模型用量 stay adjacent (§2-14).
      en['manage.logs'],
      en['manage.reliability'],
      en['manage.inference'],
      // 本地模型市集 rides directly after 推理設定 — same mental bucket
      // (local model runtime), install surface next to its settings.
      en['manage.localModels'],
      en['manage.migrate'],
      // catch-all last.
      en['manage.system'],
    ]);
  });

  it('redirects bare /manage to the first surface the viewer can see', () => {
    renderManage('/manage');
    expect(screen.getByText('/manage/channels page')).toBeInTheDocument();
  });

  it('fail-closes: an employee visiting /manage is redirected home', () => {
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    renderManage('/manage/channels');
    expect(screen.getByText('Home page')).toBeInTheDocument();
  });
});
