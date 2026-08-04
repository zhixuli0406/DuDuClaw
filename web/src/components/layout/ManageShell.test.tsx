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

  // 2026-08-04 (WP14): operator-grade diagnostics leave the Personal rail. The
  // routes stay reachable — only the advanced sub-list stops advertising them.
  it('hides 安全 / 可靠性 / 日誌 on the personal edition and keeps them on enterprise', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    const { unmount } = renderManage('/manage/billing');
    expect(screen.queryByRole('link', { name: en['manage.security'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.reliability'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.logs'] })).not.toBeInTheDocument();
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
