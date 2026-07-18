import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { ManageShell } from './ManageShell';
import { manageNav } from './nav-model';

/** Render ManageShell with a real nested-route tree (NavLink + Outlet need it —
 *  Zone D is real routing, not `?tab=`, per WP4.1). */
function renderManage(initialPath: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route path="/" element={<div>Home page</div>} />
          <Route path="manage" element={<ManageShell />}>
            {manageNav.map((item) => (
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

describe('ManageShell (Multica Settings-式 rail, WP4.1)', () => {
  it('groups the rail into 營運 / 帳務與授權 / 治理 with group labels', () => {
    renderManage('/manage/channels');
    expect(screen.getByText('Operations')).toBeInTheDocument();
    expect(screen.getByText('Billing & licensing')).toBeInTheDocument();
    // "Governance" appears twice — the group label and the manage.governance
    // nav item share the same English string; assert both are present.
    expect(screen.getAllByText('Governance').length).toBe(2);
  });

  it('renders every manageNav item as a link for an admin+enterprise viewer', () => {
    renderManage('/manage/channels');
    for (const item of manageNav) {
      const label = en[item.label as keyof typeof en] as string;
      expect(screen.getByRole('link', { name: label })).toBeInTheDocument();
    }
  });

  it('marks the active route with aria-current and the selected-surface class', () => {
    renderManage('/manage/channels');
    const active = screen.getByRole('link', { name: en['manage.channels'] });
    expect(active).toHaveAttribute('aria-current', 'page');
    expect(active.className).toContain('bg-surface-selected');

    const inactive = screen.getByRole('link', { name: en['manage.billing'] });
    expect(inactive).not.toHaveAttribute('aria-current', 'page');
    expect(inactive.className).toContain('text-muted-foreground');
  });

  it('renders the routed child page inside the content pane', () => {
    renderManage('/manage/billing');
    expect(screen.getByText('/manage/billing page')).toBeInTheDocument();
  });

  it('hides admin-gated items and collapses the now-empty Operations group for a manager-only viewer', () => {
    useAuthStore.setState({ user: { display_name: 'M', role: 'manager' } as never });
    renderManage('/manage/billing');
    // Every Operations item (channels/integrations/inference/system) requires
    // admin, so the whole group — including its label — disappears.
    expect(screen.queryByText('Operations')).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.channels'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.system'] })).not.toBeInTheDocument();
    // Manager-visible items remain in their groups.
    expect(screen.getByText('Billing & licensing')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.billing'] })).toBeInTheDocument();
    expect(screen.getByText('Governance')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: en['manage.logs'] })).toBeInTheDocument();
  });

  it('hides enterprise-only items on the personal edition', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderManage('/manage/channels');
    expect(screen.queryByRole('link', { name: en['manage.governance'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.users'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: en['manage.departments'] })).not.toBeInTheDocument();
    // Non-enterprise-gated items in the same group stay visible.
    expect(screen.getByRole('link', { name: en['manage.security'] })).toBeInTheDocument();
  });

  it('redirects bare /manage to the first surface the viewer can see', () => {
    renderManage('/manage');
    expect(screen.getByText('/manage/channels page')).toBeInTheDocument();
  });

  it('fail-closes: an employee visiting /manage is redirected home', () => {
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    renderManage('/manage/channels');
    expect(screen.getByText('Home page')).toBeInTheDocument();
    expect(screen.queryByText('Operations')).not.toBeInTheDocument();
  });
});
